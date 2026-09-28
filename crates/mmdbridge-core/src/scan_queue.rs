use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
};

use chrono::Utc;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    CoreError, CoreResult, Library,
    types::{ScanChange, ScanChangeKind, ScanState},
};

static WORKER: OnceLock<Result<Arc<ScanQueue>, String>> = OnceLock::new();

struct ScanQueue {
    pending: Mutex<PendingScans>,
    changed: Condvar,
}

struct PendingScans {
    tasks: VecDeque<(Library, String)>,
}

fn worker_queue() -> CoreResult<Arc<ScanQueue>> {
    WORKER
        .get_or_init(|| {
            let queue = Arc::new(ScanQueue {
                pending: Mutex::new(PendingScans {
                    tasks: VecDeque::new(),
                }),
                changed: Condvar::new(),
            });
            let worker_queue = Arc::clone(&queue);
            thread::Builder::new()
                .name("mmdbridge-scan-worker".to_owned())
                .spawn(move || worker_loop(worker_queue))
                .map_err(|error| error.to_string())?;
            Ok(queue)
        })
        .clone()
        .map_err(CoreError::InvalidRoot)
}

fn push(queue: &ScanQueue, library: &Library, root_id: &str) -> CoreResult<()> {
    let mut pending = queue.pending.lock().map_err(|_| CoreError::LockPoisoned)?;
    if !pending.tasks.iter().any(|(_, queued_id)| queued_id == root_id) {
        pending.tasks.push_back((library.clone(), root_id.to_owned()));
        queue.changed.notify_one();
    }
    Ok(())
}

pub(crate) fn enqueue(library: &Library, root_id: &str, full_check: bool) -> CoreResult<ScanState> {
    let root = library.list_roots()?.into_iter()
        .find(|root| root.id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
    if !root.enabled {
        return Err(CoreError::RootDisabled(root_id.to_owned()));
    }
    let queue = worker_queue()?;
    let now = Utc::now().to_rfc3339();
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let status: Option<String> = transaction.query_row(
        "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
    ).optional()?;
    let can_enqueue = !matches!(status.as_deref(), Some(
        "Pending" | "Paused" | "Pausing" | "Discovering" | "Indexing" | "Verifying" |
        "Relations" | "Cancelling"
    ));
    let should_queue = can_enqueue || status.as_deref() == Some("Pending");
    let has_root_change: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM scan_changes WHERE root_id=?1 AND scope='root')",
        [root_id], |row| row.get(0),
    )?;
    let active = matches!(status.as_deref(), Some("Discovering" | "Indexing" | "Verifying" | "Relations"));
    let paused_or_pending = matches!(status.as_deref(), Some("Pending" | "Paused" | "Pausing"));
    let should_record_root = can_enqueue || active || (paused_or_pending && !has_root_change);
    if should_record_root {
        let queue_order: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(queue_order),0)+1 FROM scan_state", [], |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO scan_state(root_id,status,progress,error_json,updated_at,files_seen,files_processed,queue_order,full_check,dirty_generation)
             VALUES (?1,'Pending',0,NULL,?2,0,0,?3,?4,1)
             ON CONFLICT(root_id) DO UPDATE SET
               status=CASE WHEN scan_state.status IN ('Paused','Pausing','Discovering','Indexing','Verifying','Relations')
                           THEN scan_state.status ELSE 'Pending' END,
               progress=CASE WHEN scan_state.status IN ('Discovering','Indexing','Verifying','Relations')
                             THEN scan_state.progress ELSE 0 END,
               error_json=NULL,
               updated_at=excluded.updated_at,files_seen=0,files_processed=0,
               queue_order=excluded.queue_order,full_check=MAX(scan_state.full_check,excluded.full_check),
               dirty_generation=scan_state.dirty_generation+1",
            params![root_id, now, queue_order, i64::from(full_check)],
        )?;
        let generation: i64 = transaction.query_row(
            "SELECT dirty_generation FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO scan_changes(root_id,path_key,path,scope,generation,updated_at)
             VALUES (?1,'','root','root',?2,?3)
             ON CONFLICT(root_id,path_key) DO UPDATE SET path='root',scope='root',generation=excluded.generation,updated_at=excluded.updated_at",
            params![root_id, generation, now],
        )?;
        if should_queue {
            transaction.execute("UPDATE roots SET scan_status='Pending' WHERE id=?1", [root_id])?;
        }
    }
    transaction.commit()?;
    drop(connection);
    if should_queue { push(&queue, library, root_id)?; }
    library.list_scan_states()?.into_iter()
        .find(|state| state.root_id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))
}

pub(crate) fn enqueue_changes(
    library: &Library,
    root_id: &str,
    changes: &[ScanChange],
) -> CoreResult<Option<ScanState>> {
    let root = library.list_roots()?.into_iter()
        .find(|root| root.id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
    if !root.enabled { return Err(CoreError::RootDisabled(root_id.to_owned())); }
    let root_key = crate::scanner::scan_path_key(std::path::Path::new(&root.path));
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    let mut normalized = Vec::<(String, String, String)>::new();
    for change in changes {
        if change.kind == ScanChangeKind::Root {
            normalized.push((String::new(), "root".to_owned(), "root".to_owned()));
            continue;
        }
        let path = std::path::Path::new(&change.path);
        let key = crate::scanner::scan_path_key(path);
        if key != root_key && !key.strip_prefix(&root_key).is_some_and(|suffix| suffix.starts_with('/')) {
            continue;
        }
        if !root.scan_recursive {
            let direct_file = path.parent().is_some_and(|parent| {
                crate::scanner::scan_path_key(parent) == root_key
            });
            let root_directory_event = key == root_key
                && matches!(change.kind, ScanChangeKind::Subtree | ScanChangeKind::Removed);
            let nested_event = key != root_key
                && key.strip_prefix(&root_key).is_some_and(|suffix| suffix.starts_with('/'))
                && matches!(change.kind, ScanChangeKind::File | ScanChangeKind::Subtree | ScanChangeKind::Removed);
            if !direct_file && !root_directory_event && !nested_event { continue; }
        }
        let scope = match change.kind {
            ScanChangeKind::File => "file",
            ScanChangeKind::Subtree if !root.scan_recursive || key == root_key => "root",
            ScanChangeKind::Subtree if path.is_dir() => "subtree",
            ScanChangeKind::Subtree => "root",
            ScanChangeKind::Removed => {
                let exact: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM assets a JOIN asset_files f ON f.asset_id=a.id
                     WHERE a.root_id=?1 AND a.retired_format=0 AND f.path_key=?2)",
                    params![root_id, key], |row| row.get(0),
                )?;
                let descendant: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM assets a JOIN asset_files f ON f.asset_id=a.id
                     WHERE a.root_id=?1 AND a.retired_format=0 AND substr(f.path_key,1,length(?2)+1)=?2 || '/')",
                    params![root_id, key], |row| row.get(0),
                )?;
                if exact { "file" } else if descendant { "root" }
                else if root.asset_type.accepts_extension(path.extension().and_then(|value| value.to_str()).unwrap_or_default()) { "file" }
                else if path.extension().is_none() { "root" }
                else { continue }
            }
            ScanChangeKind::Root => "root",
        };
        if scope == "file" {
            let references: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM assets a JOIN asset_files f ON f.asset_id=a.id
                 WHERE a.root_id=?1 AND a.retired_format=0 AND f.path_key=?2)",
                params![root_id, key], |row| row.get(0),
            )?;
            let is_primary = root.asset_type.accepts_extension(
                path.extension().and_then(|value| value.to_str()).unwrap_or_default(),
            );
            let nested_nonrecursive_file = !root.scan_recursive && path.parent().is_some_and(|parent| {
                crate::scanner::scan_path_key(parent) != root_key
            });
            if !references && (!is_primary || nested_nonrecursive_file) {
                let missing_dependency: bool = transaction.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM assets a
                       JOIN metadata m ON m.asset_id=a.id AND m.key='parsed'
                       JOIN json_each(CASE WHEN json_valid(m.value_json)
                         THEN COALESCE(json_extract(m.value_json,'$.file_dependencies'),'[]')
                         ELSE '[]' END) dependency
                       WHERE a.root_id=?1 AND a.retired_format=0
                         AND json_extract(dependency.value,'$.status')='missing'
                         AND lower(replace(json_extract(dependency.value,'$.path'),'\\','/'))=?2
                     )",
                    params![root_id, key], |row| row.get(0),
                )?;
                if missing_dependency || is_possible_mmd_dependency(path) {
                    normalized.push((String::new(), "root".to_owned(), "root".to_owned()));
                }
                continue;
            }
        }
        normalized.push((
            if scope == "root" { String::new() } else { key },
            if scope == "root" { "root".to_owned() } else { change.path.clone() },
            scope.to_owned(),
        ));
    }
    if normalized.is_empty() { return Ok(None); }

    let queue_order: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(queue_order),0)+1 FROM scan_state", [], |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO scan_state(root_id,status,updated_at,queue_order,dirty_generation)
         VALUES (?1,'Pending',?2,?3,1)
         ON CONFLICT(root_id) DO UPDATE SET dirty_generation=scan_state.dirty_generation+1,updated_at=excluded.updated_at",
        params![root_id, now, queue_order],
    )?;
    let generation: i64 = transaction.query_row(
        "SELECT dirty_generation FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
    )?;
    for (path_key, path, scope) in normalized {
        transaction.execute(
            "INSERT INTO scan_changes(root_id,path_key,path,scope,generation,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(root_id,path_key) DO UPDATE SET
               path=excluded.path,
               scope=CASE WHEN scan_changes.scope='root' OR excluded.scope='root' THEN 'root'
                          WHEN scan_changes.scope='subtree' OR excluded.scope='subtree' THEN 'subtree'
                          ELSE 'file' END,
               generation=excluded.generation,updated_at=excluded.updated_at",
            params![root_id, path_key, path, scope, generation, now],
        )?;
    }
    let current_status: String = transaction.query_row(
        "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
    )?;
    let should_queue = !matches!(current_status.as_str(), "Paused" | "Pausing" | "Cancelling")
        && !matches!(current_status.as_str(), "Discovering" | "Indexing" | "Verifying" | "Relations");
    if should_queue {
        transaction.execute(
            "UPDATE scan_state SET status='Pending',progress=0,error_json=NULL,files_seen=0,
             files_processed=0,queue_order=?2 WHERE root_id=?1 AND status NOT IN ('Pending')",
            params![root_id, queue_order],
        )?;
        transaction.execute("UPDATE roots SET scan_status='Pending' WHERE id=?1", [root_id])?;
    }
    transaction.commit()?;
    drop(connection);
    if should_queue {
        let queue = worker_queue()?;
        push(&queue, library, root_id)?;
    }
    Ok(library.list_scan_states()?.into_iter().find(|state| state.root_id == root_id))
}

fn is_possible_mmd_dependency(path: &std::path::Path) -> bool {
    path.extension().and_then(|value| value.to_str()).is_some_and(|extension| {
        ["png", "jpg", "jpeg", "bmp", "tga", "dds", "webp", "spa", "sph", "toon"]
            .iter()
            .any(|supported| extension.eq_ignore_ascii_case(supported))
    })
}

pub(crate) fn resume(library: &Library) -> CoreResult<()> {
    let queue = worker_queue()?;
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active = {
        let mut statement = transaction.prepare(
            "SELECT root_id,status,claim_owner FROM scan_state
             WHERE status IN ('Pausing','Cancelling','Discovering','Indexing','Verifying','Relations')",
        )?;
        statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?))
        })?.collect::<Result<Vec<_>, _>>()?
    };
    for (root_id, status, owner) in active {
        if claim_owner_is_live(owner) { continue; }
        let recovered_status = match status.as_str() {
            "Pausing" => "Paused",
            "Cancelling" => "Cancelled",
            _ => "Pending",
        };
        transaction.execute(
            "UPDATE scan_state SET status=?2,claim_owner=NULL,progress=CASE WHEN ?2='Pending' THEN 0 ELSE progress END,
             files_seen=CASE WHEN ?2='Pending' THEN 0 ELSE files_seen END,
             files_processed=CASE WHEN ?2='Pending' THEN 0 ELSE files_processed END,updated_at=?3
             WHERE root_id=?1 AND status=?4",
            params![root_id, recovered_status, Utc::now().to_rfc3339(), status],
        )?;
        transaction.execute(
            "UPDATE roots SET scan_status=?2 WHERE id=?1",
            params![root_id, recovered_status],
        )?;
    }
    transaction.execute(
        "UPDATE scan_state SET status='Pending',progress=0,files_seen=0,files_processed=0,
         updated_at=?1 WHERE status='Completed' AND EXISTS(
           SELECT 1 FROM scan_changes c WHERE c.root_id=scan_state.root_id)",
        [Utc::now().to_rfc3339()],
    )?;
    transaction.execute("UPDATE roots SET scan_status='Pending' WHERE id IN
        (SELECT root_id FROM scan_state WHERE status='Pending')", [])?;
    let pending = {
        let mut statement = transaction.prepare("SELECT root_id FROM scan_state WHERE status='Pending'")?;
        statement.query_map([], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?
    };
    transaction.commit()?;
    for root_id in pending { push(&queue, library, &root_id)?; }
    Ok(())
}

fn claim_owner_is_live(owner: Option<i64>) -> bool {
    match owner {
        Some(owner) if owner == i64::from(std::process::id()) => true,
        Some(owner) => process_is_alive(owner).unwrap_or(true),
        None => false,
    }
}

#[cfg(windows)]
fn process_is_alive(process_id: i64) -> Option<bool> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    if process_id <= 0 { return Some(false); }
    let process = match unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id as u32)
    } {
        Ok(process) => process,
        Err(error) if error.code().0 == 0x8007_0057_u32 as i32 => return Some(false),
        Err(_) => return None,
    };
    let mut exit_code = 0_u32;
    let status = unsafe { GetExitCodeProcess(process, &mut exit_code) };
    let _ = unsafe { CloseHandle(process) };
    status.is_ok().then_some(exit_code == 259)
}

#[cfg(target_os = "linux")]
fn process_is_alive(process_id: i64) -> Option<bool> {
    Some(process_id > 0 && std::path::Path::new("/proc").join(process_id.to_string()).exists())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn process_is_alive(process_id: i64) -> Option<bool> {
    (process_id == i64::from(std::process::id())).then_some(true)
}

pub(crate) fn continue_paused(library: &Library, root_id: &str) -> CoreResult<ScanState> {
    let root = library.list_roots()?.into_iter()
        .find(|root| root.id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
    if !root.enabled { return Err(CoreError::RootDisabled(root_id.to_owned())); }
    let queue = worker_queue()?;
    let changed = {
        let connection = library.connection()?;
        let queue_order: i64 = connection.query_row(
            "SELECT COALESCE(MAX(queue_order),0)+1 FROM scan_state", [], |row| row.get(0),
        )?;
        let changed = connection.execute(
            "UPDATE scan_state SET status='Pending',progress=0,files_seen=0,files_processed=0,
             error_json=NULL,updated_at=?2,queue_order=?3 WHERE root_id=?1 AND status='Paused'",
            params![root_id, Utc::now().to_rfc3339(), queue_order],
        )?;
        if changed > 0 { connection.execute("UPDATE roots SET scan_status='Pending' WHERE id=?1", [root_id])?; }
        changed
    };
    if changed == 0 { return Err(CoreError::InvalidRoot("只有已暂停的扫描可以继续".to_owned())); }
    push(&queue, library, root_id)?;
    library.list_scan_states()?.into_iter().find(|state| state.root_id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))
}

pub(crate) fn move_pending(library: &Library, root_id: &str, direction: i32) -> CoreResult<bool> {
    if direction != -1 && direction != 1 {
        return Err(CoreError::InvalidRoot("队列排序方向必须为 -1 或 1".to_owned()));
    }
    let ordered = {
        let mut connection = library.connection()?;
        let transaction = connection.transaction()?;
        let mut ordered = {
            let mut statement = transaction.prepare(
                "SELECT root_id FROM scan_state WHERE status='Pending' ORDER BY queue_order,updated_at,root_id",
            )?;
            statement.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let Some(index) = ordered.iter().position(|id| id == root_id) else { return Ok(false); };
        let other = index as i32 + direction;
        if other < 0 || other >= ordered.len() as i32 { return Ok(false); }
        ordered.swap(index, other as usize);
        for (index, id) in ordered.iter().enumerate() {
            transaction.execute("UPDATE scan_state SET queue_order=?2 WHERE root_id=?1", params![id, index as i64 + 1])?;
        }
        transaction.commit()?;
        ordered
    };
    let queue = worker_queue()?;
    let mut pending = queue.pending.lock().map_err(|_| CoreError::LockPoisoned)?;
    pending.tasks.make_contiguous().sort_by_key(|(_, id)| {
        ordered.iter().position(|item| item == id).unwrap_or(usize::MAX)
    });
    Ok(true)
}

fn worker_loop(queue: Arc<ScanQueue>) {
    loop {
        let (library, root_id) = {
            let Ok(pending) = queue.pending.lock() else { return; };
            let Ok(mut pending) = queue.changed.wait_while(pending, |pending| pending.tasks.is_empty()) else { return; };
            pending.tasks.pop_front().expect("nonempty scan queue")
        };
        let work = library.claim_pending_scan(&root_id).unwrap_or(None);
        if let Some(work) = work {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                library.scan_queued_root(&root_id, &work)
            }));
            match result {
                Ok(Err(CoreError::RootDisabled(_))) => {
                    let _ = library.finish_scan(&root_id, &work, &Err(CoreError::ScanCancelled));
                }
                Ok(Err(CoreError::ScanPaused | CoreError::ScanCancelled)) => {}
                Ok(Err(error)) => eprintln!("MMDbridgeLib 扫描失败（{root_id}）：{error}"),
                Err(_) => {
                    let error = CoreError::Io(std::io::Error::other("扫描工作线程发生异常"));
                    let _ = library.finish_scan(&root_id, &work, &Err(error));
                }
                Ok(Ok(_)) => {}
            }
            if library.list_scan_states().ok().is_some_and(|states| {
                states.iter().any(|state| state.root_id == root_id && state.status == "Pending")
            }) {
                let _ = push(&queue, &library, &root_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetType;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    #[test]
    fn queued_empty_root_reports_completion() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-scan-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let library = Library::in_memory().unwrap();
        let root = library.add_root(AssetType::Model, directory.to_str().unwrap(), None).unwrap();
        library.enqueue_scan(&root.id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = library.list_scan_states().unwrap().into_iter().next().unwrap();
            if state.status == "Completed" {
                assert_eq!(state.files_seen, 0);
                assert_eq!(state.progress, 1.0);
                break;
            }
            assert_ne!(state.status, "Failed", "scan failed: {:?}", state.error);
            assert!(Instant::now() < deadline, "queued scan did not finish");
            thread::sleep(Duration::from_millis(25));
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn pause_resume_and_order_are_persisted() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-scan-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let library = Library::in_memory().unwrap();
        let first = library.add_root(AssetType::Model, directory.to_str().unwrap(), None).unwrap();
        let second = library.add_root(AssetType::Scene, directory.to_str().unwrap(), None).unwrap();
        {
            let connection = library.connection().unwrap();
            for (root, order) in [(&first, 1), (&second, 2)] {
                connection.execute(
                    "INSERT INTO scan_state(root_id,status,progress,error_json,updated_at,queue_order,dirty_generation)
                     VALUES (?1,'Pending',0,NULL,?2,?3,1)",
                    params![root.id, Utc::now().to_rfc3339(), order],
                ).unwrap();
                connection.execute(
                    "INSERT INTO scan_changes(root_id,path_key,path,scope,generation,updated_at)
                     VALUES (?1,'','root','root',1,?2)",
                    params![root.id, Utc::now().to_rfc3339()],
                ).unwrap();
            }
        }
        assert!(library.move_pending_scan(&second.id, -1).unwrap());
        assert_eq!(library.list_scan_states().unwrap()[0].root_id, second.id);
        assert!(library.pause_scan(&second.id).unwrap());
        assert_eq!(library.list_scan_states().unwrap().iter().find(|state| state.root_id == second.id).unwrap().status, "Paused");
        let work = library.claim_pending_scan(&first.id).unwrap().unwrap();
        assert!(library.pause_scan(&first.id).unwrap());
        assert!(matches!(library.update_scan_progress(&first.id, "Indexing", 0.2, 10, 2), Err(CoreError::ScanPaused)));
        assert!(library.cancel_scan(&first.id).unwrap());
        library.finish_scan(&first.id, &work, &Err(CoreError::ScanPaused)).unwrap();
        assert_eq!(library.list_scan_states().unwrap().iter().find(|state| state.root_id == first.id).unwrap().status, "Cancelled");
        library.continue_scan(&second.id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = library.list_scan_states().unwrap().into_iter()
                .find(|state| state.root_id == second.id).unwrap();
            if state.status == "Completed" { break; }
            assert!(Instant::now() < deadline, "resumed scan did not complete");
            thread::sleep(Duration::from_millis(25));
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
