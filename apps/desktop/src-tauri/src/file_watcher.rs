use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use mmdbridge_core::Library;
use notify::{RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter};

const EVENT_DEBOUNCE: Duration = Duration::from_millis(900);
const ROOT_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

struct WatchedRoot {
    path: PathBuf,
    path_key: String,
    recursive: bool,
}

pub fn start(app: AppHandle, library: Library) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    let watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .map_err(|error| io::Error::other(format!("无法初始化文件监视器：{error}")))?;

    thread::Builder::new()
        .name("mmdbridge-file-watcher".to_owned())
        .spawn(move || watch_loop(app, library, watcher, receiver))
        .map(|_| ())
}

fn watch_loop(
    app: AppHandle,
    library: Library,
    mut watcher: notify::RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
) {
    let mut watched = HashMap::<String, WatchedRoot>::new();
    let mut reported_watch_errors = HashSet::<String>::new();
    let mut pending_roots = HashMap::<String, Instant>::new();
    let mut last_root_sync = Instant::now() - ROOT_SYNC_INTERVAL;

    loop {
        match receiver.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok(Ok(event)) => {
                if event_relevant(&event) {
                    let now = Instant::now();
                    for root_id in roots_for_event(&event, &watched) {
                        pending_roots.insert(root_id, now);
                    }
                }
            }
            Ok(Err(error)) => eprintln!("MMDbridgeLib 文件监视事件失败：{error}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        if last_root_sync.elapsed() >= ROOT_SYNC_INTERVAL {
            match sync_roots(
                &app,
                &library,
                &mut watcher,
                &mut watched,
                &mut reported_watch_errors,
            ) {
                Ok(()) => {
                    pending_roots.retain(|root_id, _| watched.contains_key(root_id));
                }
                Err(error) => eprintln!("MMDbridgeLib 根目录监视同步失败：{error}"),
            }
            last_root_sync = Instant::now();
        }

        let now = Instant::now();
        let ready = pending_roots
            .iter()
            .filter_map(|(root_id, changed_at)| {
                (now.duration_since(*changed_at) >= EVENT_DEBOUNCE).then(|| root_id.clone())
            })
            .collect::<Vec<_>>();
        for root_id in ready {
            pending_roots.remove(&root_id);
            match library.enqueue_scan(&root_id) {
                Ok(_) => {}
                Err(error) => {
                    if !matches!(&error, mmdbridge_core::CoreError::RootDisabled(_)) {
                        eprintln!("MMDbridgeLib 自动扫描失败：{error}");
                        let _ = app.emit(
                            "library-watch-error",
                            serde_json::json!({"rootId": root_id, "message": error.to_string()}),
                        );
                    }
                }
            }
        }
    }
}

fn sync_roots(
    app: &AppHandle,
    library: &Library,
    watcher: &mut notify::RecommendedWatcher,
    watched: &mut HashMap<String, WatchedRoot>,
    reported_errors: &mut HashSet<String>,
) -> Result<(), String> {
    let roots = library.list_roots().map_err(|error| error.to_string())?;
    let desired = roots
        .into_iter()
        .filter(|root| root.enabled)
        .map(|root| (root.id.clone(), root))
        .collect::<HashMap<_, _>>();

    let remove_ids = watched
        .iter()
        .filter_map(|(root_id, current)| {
            let still_matches = desired.get(root_id).is_some_and(|root| {
                current.path_key == path_key(Path::new(&root.path))
                    && current.recursive == root.scan_recursive
            });
            (!still_matches).then(|| root_id.clone())
        })
        .collect::<Vec<_>>();
    for root_id in remove_ids {
        if let Some(root) = watched.remove(&root_id) {
            if let Err(error) = watcher.unwatch(&root.path) {
                eprintln!(
                    "MMDbridgeLib 停止监视目录失败：{}：{error}",
                    root.path.display()
                );
            }
        }
        reported_errors.remove(&root_id);
    }

    for (root_id, root) in desired {
        if watched.contains_key(&root_id) {
            continue;
        }
        let path = PathBuf::from(&root.path);
        let recursive_mode = if root.scan_recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        match watcher.watch(&path, recursive_mode) {
            Ok(()) => {
                watched.insert(
                    root_id.clone(),
                    WatchedRoot {
                        path_key: path_key(&path),
                        path,
                        recursive: root.scan_recursive,
                    },
                );
                reported_errors.remove(&root_id);
            }
            Err(error) => {
                if reported_errors.insert(root_id.clone()) {
                    let _ = app.emit(
                        "library-watch-error",
                        serde_json::json!({"rootId": root_id, "message": error.to_string()}),
                    );
                }
            }
        }
    }
    Ok(())
}

fn roots_for_event(
    event: &notify::Event,
    watched: &HashMap<String, WatchedRoot>,
) -> HashSet<String> {
    event
        .paths
        .iter()
        .filter(|path| !is_mmdrcv_sidecar(path))
        .flat_map(|path| {
            let changed_path = path_key(path);
            watched.iter().filter_map(move |(root_id, root)| {
                path_is_within(&changed_path, &root.path_key).then(|| root_id.clone())
            })
        })
        .collect()
}

fn event_relevant(event: &notify::Event) -> bool {
    event
        .paths
        .iter()
        .any(|path| !is_mmdrcv_sidecar(path) && is_indexable_change(path))
}

fn is_indexable_change(path: &Path) -> bool {
    if path.is_dir() || path.extension().is_none() {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["pmx", "pmd", "vmd", "vpd", "x"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn is_mmdrcv_sidecar(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mmdrcv"))
}

fn path_key(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(path_without_prefix) = normalized.strip_prefix("\\\\?\\unc\\") {
        normalized = format!("\\\\{path_without_prefix}");
    } else if let Some(path_without_prefix) = normalized.strip_prefix("\\\\?\\") {
        normalized = path_without_prefix.to_owned();
    }
    normalized.trim_end_matches('\\').to_owned()
}

fn path_is_within(changed_path: &str, root_path: &str) -> bool {
    changed_path == root_path
        || changed_path
            .strip_prefix(root_path)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}
