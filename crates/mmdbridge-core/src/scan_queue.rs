use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
};

use chrono::Utc;
use rusqlite::params;

use crate::{CoreError, CoreResult, Library, types::ScanState};

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
    let changed = {
        let connection = library.connection()?;
        let queue_order: i64 = connection.query_row(
            "SELECT COALESCE(MAX(queue_order),0)+1 FROM scan_state", [], |row| row.get(0),
        )?;
        let changed = connection.execute(
            "INSERT INTO scan_state(root_id,status,progress,error_json,updated_at,files_seen,files_processed,queue_order,full_check)
             VALUES (?1,'Pending',0,NULL,?2,0,0,?3,?4)
             ON CONFLICT(root_id) DO UPDATE SET status='Pending',progress=0,error_json=NULL,
               updated_at=excluded.updated_at,files_seen=0,files_processed=0,
               queue_order=excluded.queue_order,full_check=excluded.full_check
             WHERE scan_state.status NOT IN ('Pending','Paused','Pausing','Discovering','Indexing','Verifying','Relations','Duplicates','Cancelling')",
            params![root_id, now, queue_order, i64::from(full_check)],
        )?;
        if changed > 0 {
            connection.execute("UPDATE roots SET scan_status='Pending' WHERE id=?1", [root_id])?;
        }
        changed
    };
    if changed > 0 { push(&queue, library, root_id)?; }
    library.list_scan_states()?.into_iter()
        .find(|state| state.root_id == root_id)
        .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))
}

pub(crate) fn resume(library: &Library) -> CoreResult<()> {
    let connection = library.connection()?;
    connection.execute("UPDATE scan_state SET status='Cancelled' WHERE status='Cancelling'", [])?;
    connection.execute(
        "UPDATE scan_state SET status='Paused' WHERE status IN
         ('Pending','Pausing','Discovering','Indexing','Verifying','Relations','Duplicates')", [],
    )?;
    connection.execute("UPDATE roots SET scan_status='Paused' WHERE id IN
        (SELECT root_id FROM scan_state WHERE status='Paused')", [])?;
    Ok(())
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
        let should_run = library.claim_pending_scan(&root_id).unwrap_or(false);
        if should_run {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| library.scan_queued_root(&root_id)));
            match result {
                Ok(Err(CoreError::RootDisabled(_))) => { let _ = library.cancel_scan(&root_id); }
                Ok(Err(CoreError::ScanPaused | CoreError::ScanCancelled)) => {}
                Ok(Err(error)) => eprintln!("MMDbridgeLib 扫描失败（{root_id}）：{error}"),
                Err(_) => {
                    let error = CoreError::Io(std::io::Error::other("扫描工作线程发生异常"));
                    let _ = library.finish_scan(&root_id, &Err(error));
                }
                Ok(Ok(_)) => {}
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
                    "INSERT INTO scan_state(root_id,status,progress,error_json,updated_at,queue_order)
                     VALUES (?1,'Pending',0,NULL,?2,?3)",
                    params![root.id, Utc::now().to_rfc3339(), order],
                ).unwrap();
            }
        }
        assert!(library.move_pending_scan(&second.id, -1).unwrap());
        assert_eq!(library.list_scan_states().unwrap()[0].root_id, second.id);
        assert!(library.pause_scan(&second.id).unwrap());
        assert_eq!(library.list_scan_states().unwrap().iter().find(|state| state.root_id == second.id).unwrap().status, "Paused");
        library.begin_scan(&first.id).unwrap();
        assert!(library.pause_scan(&first.id).unwrap());
        assert!(matches!(library.update_scan_progress(&first.id, "Indexing", 0.2, 10, 2), Err(CoreError::ScanPaused)));
        assert!(library.cancel_scan(&first.id).unwrap());
        library.finish_scan(&first.id, &Err(CoreError::ScanPaused)).unwrap();
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
