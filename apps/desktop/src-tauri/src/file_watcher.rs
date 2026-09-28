use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use mmdbridge_core::{Library, ScanChange, ScanChangeKind};
use notify::{RecursiveMode, Watcher, event::{EventKind, ModifyKind}};
use tauri::{AppHandle, Emitter};

const EVENT_DEBOUNCE: Duration = Duration::from_millis(900);
const ROOT_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

struct WatchedRoot {
    path: PathBuf,
    path_key: String,
    recursive: bool,
}

struct PendingRootChanges {
    changed_at: Instant,
    changes: Vec<ScanChange>,
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
    let mut queued_initial_scans = HashSet::<String>::new();
    let mut reported_startup_scan_errors = HashSet::<String>::new();
    let mut reported_change_scan_errors = HashSet::<String>::new();
    let mut pending_roots = HashMap::<String, PendingRootChanges>::new();
    let mut last_root_sync = Instant::now() - ROOT_SYNC_INTERVAL;

    loop {
        match receiver.recv_timeout(EVENT_POLL_INTERVAL) {
            Ok(Ok(event)) => {
                if !matches!(&event.kind, EventKind::Access(_)) {
                    let now = Instant::now();
                    let fallback = event.paths.is_empty()
                        || matches!(&event.kind, EventKind::Any | EventKind::Other)
                        || matches!(&event.kind, EventKind::Modify(ModifyKind::Name(_)));
                    let affected_roots = if fallback {
                        if event.paths.is_empty() { watched.keys().cloned().collect() }
                        else { roots_for_event(&event, &watched) }
                    } else {
                        roots_for_event(&event, &watched)
                    };
                    for root_id in affected_roots {
                        let changes = if fallback {
                            vec![ScanChange { path: String::new(), kind: ScanChangeKind::Root }]
                        } else {
                            event.paths.iter().filter(|path| !is_mmdrcv_sidecar(path)).map(|path| {
                                let kind = if matches!(&event.kind, EventKind::Remove(_)) {
                                    ScanChangeKind::Removed
                                } else if path.is_dir() {
                                    ScanChangeKind::Subtree
                                } else {
                                    ScanChangeKind::File
                                };
                                ScanChange { path: path.to_string_lossy().into_owned(), kind }
                            }).collect()
                        };
                        if !changes.is_empty() {
                            let pending = pending_roots.entry(root_id).or_insert_with(|| PendingRootChanges {
                                changed_at: now,
                                changes: Vec::new(),
                            });
                            pending.changed_at = now;
                            if changes.iter().any(|change| change.kind == ScanChangeKind::Root) {
                                pending.changes.clear();
                            }
                            pending.changes.extend(changes);
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                eprintln!("MMDbridgeLib 文件监视事件失败：{error}");
                let now = Instant::now();
                for root_id in watched.keys() {
                    pending_roots.insert(root_id.clone(), PendingRootChanges {
                        changed_at: now,
                        changes: vec![ScanChange { path: String::new(), kind: ScanChangeKind::Root }],
                    });
                }
            }
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
                &mut queued_initial_scans,
                &mut reported_startup_scan_errors,
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
            .filter_map(|(root_id, pending)| {
                (now.duration_since(pending.changed_at) >= EVENT_DEBOUNCE).then(|| root_id.clone())
            })
            .collect::<Vec<_>>();
        for root_id in ready {
            let Some(pending) = pending_roots.remove(&root_id) else { continue; };
            match library.enqueue_scan_changes(&root_id, &pending.changes) {
                Ok(_) => { reported_change_scan_errors.remove(&root_id); }
                Err(error) => {
                    if matches!(
                        &error,
                        mmdbridge_core::CoreError::RootDisabled(_)
                            | mmdbridge_core::CoreError::RootNotFound(_)
                    ) {
                        reported_change_scan_errors.remove(&root_id);
                    } else {
                        pending_roots.insert(root_id.clone(), PendingRootChanges {
                            changed_at: Instant::now(),
                            changes: pending.changes,
                        });
                    }
                    if !matches!(
                        &error,
                        mmdbridge_core::CoreError::RootDisabled(_)
                            | mmdbridge_core::CoreError::RootNotFound(_)
                    ) && reported_change_scan_errors.insert(root_id.clone()) {
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
    queued_initial_scans: &mut HashSet<String>,
    reported_startup_scan_errors: &mut HashSet<String>,
) -> Result<(), String> {
    library.resume_scan_jobs().map_err(|error| error.to_string())?;
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
        queued_initial_scans.remove(&root_id);
        reported_startup_scan_errors.remove(&root_id);
    }

    for (root_id, root) in desired {
        if watched.contains_key(&root_id) && Path::new(&root.path).is_dir() {
            if !queued_initial_scans.contains(&root_id) {
                match library.enqueue_scan(&root_id) {
                    Ok(_) => {
                        queued_initial_scans.insert(root_id.clone());
                        reported_startup_scan_errors.remove(&root_id);
                    }
                    Err(error) if reported_startup_scan_errors.insert(root_id.clone()) => {
                        eprintln!("MMDbridgeLib 启动发现排队失败（{root_id}）：{error}");
                    }
                    Err(_) => {}
                }
            }
            continue;
        }
        if let Some(current) = watched.remove(&root_id) {
            let _ = watcher.unwatch(&current.path);
        }
        let path = PathBuf::from(&root.path);
        // Watch dependencies in nested folders too; Core still limits which primary files
        // a non-recursive Root may discover.
        match watcher.watch(&path, RecursiveMode::Recursive) {
            Ok(()) => {
                watched.insert(
                    root_id.clone(),
                    WatchedRoot {
                        path_key: path_key(&path),
                        path,
                        recursive: root.scan_recursive,
                    },
                );
                let recovered = reported_errors.remove(&root_id);
                if recovered { queued_initial_scans.remove(&root_id); }
                if !queued_initial_scans.contains(&root_id) {
                    match library.enqueue_scan(&root_id) {
                        Ok(_) => {
                            queued_initial_scans.insert(root_id.clone());
                            reported_startup_scan_errors.remove(&root_id);
                        }
                        Err(error) if reported_startup_scan_errors.insert(root_id.clone()) => {
                            eprintln!("MMDbridgeLib 启动发现排队失败（{root_id}）：{error}");
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(error) => {
                if reported_errors.insert(root_id.clone()) {
                    let _ = app.emit(
                        "library-watch-error",
                        serde_json::json!({"rootId": root_id, "message": error.to_string()}),
                    );
                }
                if !queued_initial_scans.contains(&root_id) {
                    match library.enqueue_scan(&root_id) {
                        Ok(_) => {
                            queued_initial_scans.insert(root_id.clone());
                            reported_startup_scan_errors.remove(&root_id);
                        }
                        Err(scan_error) if reported_startup_scan_errors.insert(root_id.clone()) => {
                            eprintln!("MMDbridgeLib 监视失败后的完整发现排队失败（{root_id}）：{scan_error}");
                        }
                        Err(_) => {}
                    }
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
