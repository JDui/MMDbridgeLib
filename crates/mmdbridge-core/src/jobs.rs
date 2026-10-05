use std::{
    cmp::Ordering as CmpOrdering,
    collections::BinaryHeap,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, CoreResult, Library};

const MAX_WORKERS: usize = 24;
const TERMINAL_STATUSES: [&str; 3] = ["Completed", "Failed", "Cancelled"];

static WORKER: OnceLock<Result<Arc<ThumbnailQueue>, String>> = OnceLock::new();
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct ThumbnailQueue {
    pending: Mutex<BinaryHeap<ThumbnailTask>>,
    changed: Condvar,
}

struct ThumbnailTask {
    library: Library,
    job_id: String,
    priority: i64,
    sequence: u64,
}

impl PartialEq for ThumbnailTask {
    fn eq(&self, other: &Self) -> bool { self.sequence == other.sequence }
}
impl Eq for ThumbnailTask {}
impl PartialOrd for ThumbnailTask {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> { Some(self.cmp(other)) }
}
impl Ord for ThumbnailTask {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.priority.cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

pub(crate) fn start_worker(library: Library) -> CoreResult<()> {
    let queue = worker_queue()?;
    let pending = {
        let connection = library.connection()?;
        recover_abandoned_jobs(&connection)?;
        let mut statement = connection.prepare(
            "SELECT id,priority FROM jobs WHERE kind='thumbnail' AND status='Pending'
             ORDER BY priority DESC,created_at ASC",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (job_id, priority) in pending {
        queue.enqueue_blocking(new_task(library.clone(), job_id, priority))?;
    }
    Ok(())
}

pub(crate) fn enqueue_thumbnail(
    library: &Library,
    asset_id: &str,
    priority: i32,
) -> CoreResult<Value> {
    Ok(enqueue_thumbnails(library, &[asset_id.to_owned()], priority)?.remove(0))
}

fn active_job(connection: &Connection, asset_id: &str) -> CoreResult<Option<String>> {
    Ok(connection.query_row(
        "SELECT id FROM jobs WHERE asset_id=?1 AND kind='thumbnail'
         AND status IN ('Pending','Parsing','Rendering','Encoding','Cancelling')
         ORDER BY created_at,id LIMIT 1",
        [asset_id], |row| row.get(0),
    ).optional()?)
}

fn reserve_thumbnails(library: &Library, asset_ids: &[String], priority: i32) -> CoreResult<Vec<(String, bool)>> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let reserved = asset_ids.iter().map(|id| reserve_thumbnail_on(&transaction, id, priority)).collect::<CoreResult<Vec<_>>>()?;
    transaction.commit()?;
    Ok(reserved)
}

#[cfg(test)]
fn reserve_thumbnail(library: &Library, asset_id: &str, priority: i32) -> CoreResult<(String, bool)> {
    Ok(reserve_thumbnails(library, &[asset_id.to_owned()], priority)?.remove(0))
}

fn reserve_thumbnail_on(connection: &Connection, asset_id: &str, priority: i32) -> CoreResult<(String, bool)> {
    if let Some(id) = active_job(connection, asset_id)? { return Ok((id, false)); }
    if crate::operations::is_asset_operation_active_on(connection, asset_id)? {
        return Err(CoreError::ThumbnailQueue("该资产正在执行文件操作，暂时不能生成缩略图".to_owned()));
    }
    let visible: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1 AND retired_format=0 AND visibility='normal')",
        [asset_id], |row| row.get(0),
    )?;
    if !visible { return Err(CoreError::AssetNotFound(asset_id.to_owned())); }
    let id = Uuid::new_v4().to_string();
    connection.execute(
        "INSERT INTO jobs(id,asset_id,kind,priority,status,progress,error_json,created_at,updated_at)
         VALUES (?1,?2,'thumbnail',?3,'Pending',0,NULL,?4,?4)",
        params![id, asset_id, priority, Utc::now().to_rfc3339()],
    )?;
    Ok((id, true))
}

fn recover_abandoned_jobs(connection: &Connection) -> CoreResult<()> {
    let active = connection.prepare(
        "SELECT id,status,claim_owner,updated_at FROM jobs WHERE kind='thumbnail'
         AND status IN ('Parsing','Rendering','Encoding','Cancelling')",
    )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?, row.get::<_, String>(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let legacy_cutoff = (Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
    for (id, status, owner, updated_at) in active {
        if crate::scan_queue::claim_owner_is_live(owner) || (owner.is_none() && updated_at >= legacy_cutoff) { continue; }
        let recovered = if status == "Cancelling" { "Cancelled" } else { "Pending" };
        connection.execute(
            "UPDATE jobs SET status=?2,claim_owner=NULL,progress=CASE WHEN ?2='Pending' THEN 0 ELSE progress END,updated_at=?3
             WHERE id=?1 AND status=?4 AND claim_owner IS ?5",
            params![id, recovered, Utc::now().to_rfc3339(), status, owner],
        )?;
    }
    Ok(())
}

pub(crate) fn enqueue_thumbnails(
    library: &Library,
    asset_ids: &[String],
    priority: i32,
) -> CoreResult<Vec<Value>> {
    for asset_id in asset_ids { library.inspect_asset(asset_id)?; }
    let reserved = reserve_thumbnails(library, asset_ids, priority)?;
    let tasks = reserved.iter().filter(|(_, created)| *created)
        .map(|(id, _)| new_task(library.clone(), id.clone(), i64::from(priority))).collect::<Vec<_>>();
    if !tasks.is_empty() {
        if let Err(error) = worker_queue().and_then(|queue| queue.try_enqueue_many(tasks).map_err(CoreError::ThumbnailQueue)) {
            for (id, created) in &reserved {
                if *created { update_terminal(library, id, "Failed", &error.to_string())?; }
            }
            return Err(error);
        }
    }
    reserved.iter().map(|(id, _)| get_job(library, id)).collect()
}

pub(crate) fn cancel_job(library: &Library, job_id: &str) -> CoreResult<bool> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE jobs SET status=CASE WHEN status='Pending' THEN 'Cancelled' ELSE 'Cancelling' END,updated_at=?1,
         error_json=json_object('message','Cancelled by user')
         WHERE id=?2 AND kind='thumbnail' AND status IN ('Pending','Parsing','Rendering','Encoding')",
        params![now, job_id],
    )?;
    if changed == 0 {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1)",
            [job_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CoreError::JobNotFound(job_id.to_owned()));
        }
    }
    Ok(changed > 0)
}

pub(crate) fn retry_job(library: &Library, job_id: &str) -> CoreResult<Value> {
    let (id, priority, queued) = reserve_retry(library, job_id)?;
    if !queued { return get_job(library, &id); }
    let task = new_task(library.clone(), id.clone(), priority);
    if let Err(error) = worker_queue().and_then(|queue| queue.try_enqueue_many(vec![task]).map_err(CoreError::ThumbnailQueue)) {
        update_terminal(library, &id, "Failed", &error.to_string())?;
        return Err(error);
    }
    get_job(library, &id)
}

fn reserve_retry(library: &Library, job_id: &str) -> CoreResult<(String, i64, bool)> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let job: Option<(String, i64, String)> = {
        transaction
            .query_row(
                "SELECT asset_id,priority,status FROM jobs WHERE id=?1 AND kind='thumbnail'",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
    };
    let Some((asset_id, priority, status)) = job else {
        return Err(CoreError::JobNotFound(job_id.to_owned()));
    };
    if crate::operations::is_asset_operation_active_on(&transaction, &asset_id)? {
        return Err(CoreError::ThumbnailQueue(
            "该资产正在执行文件操作，暂时不能重试缩略图任务".to_owned(),
        ));
    }
    if let Some(id) = active_job(&transaction, &asset_id)? { return Ok((id, priority, false)); }
    if !matches!(status.as_str(), "Failed" | "Cancelled") {
        return Err(CoreError::ThumbnailQueue(format!(
            "只能重试 Failed 或 Cancelled 任务，当前状态为 {status}"
        )));
    }
    let visible: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1 AND retired_format=0 AND visibility='normal')",
        [&asset_id], |row| row.get(0),
    )?;
    if !visible { return Err(CoreError::AssetNotFound(asset_id)); }
    transaction.execute(
        "UPDATE jobs SET status='Pending',progress=0,error_json=NULL,claim_owner=NULL,updated_at=?1 WHERE id=?2",
        params![Utc::now().to_rfc3339(), job_id],
    )?;
    transaction.commit()?;
    Ok((job_id.to_owned(), priority, true))
}

pub(crate) fn wait_for_job(
    library: &Library,
    job_id: &str,
    timeout: Duration,
) -> CoreResult<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let job = get_job(library, job_id)?;
        let status = job
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if TERMINAL_STATUSES.contains(&status) || Instant::now() >= deadline {
            return Ok(job);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn worker_queue() -> CoreResult<Arc<ThumbnailQueue>> {
    WORKER
        .get_or_init(|| {
            let queue = Arc::new(ThumbnailQueue {
                pending: Mutex::new(BinaryHeap::new()),
                changed: Condvar::new(),
            });
            for worker_id in 0..MAX_WORKERS {
                let worker_queue = Arc::clone(&queue);
                thread::Builder::new()
                    .name(format!("mmdbridge-thumbnail-worker-{worker_id:02}"))
                    .stack_size(8 * 1024 * 1024)
                    .spawn(move || worker_loop(worker_queue))
                    .map_err(|error| error.to_string())?;
            }
            Ok(queue)
        })
        .clone()
        .map_err(|error| CoreError::ThumbnailQueue(error.clone()))
}

fn new_task(library: Library, job_id: String, priority: i64) -> ThumbnailTask {
    ThumbnailTask {
        library,
        job_id,
        priority,
        sequence: NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    }
}

impl ThumbnailQueue {
    fn try_enqueue_many(&self, tasks: Vec<ThumbnailTask>) -> Result<(), String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "缩略图队列锁不可用".to_owned())?;
        pending.extend(tasks);
        self.changed.notify_all();
        Ok(())
    }

    fn enqueue_blocking(&self, task: ThumbnailTask) -> CoreResult<()> {
        let mut pending = self.pending.lock().map_err(|_| CoreError::LockPoisoned)?;
        pending.push(task);
        self.changed.notify_one();
        Ok(())
    }
}

fn worker_loop(queue: Arc<ThumbnailQueue>) {
    loop {
        let task = {
            let Ok(pending) = queue.pending.lock() else {
                return;
            };
            let mut pending = match queue
                .changed
                .wait_while(pending, |pending| pending.is_empty())
            {
                Ok(pending) => pending,
                Err(_) => return,
            };
            let task = pending.pop().expect("nonempty thumbnail queue");
            queue.changed.notify_all();
            task
        };
        run_task(task);
    }
}

fn run_task(task: ThumbnailTask) {
    let job_id = task.job_id.clone();
    let library = task.library.clone();
    if !claim_job(&library, &job_id).unwrap_or(false) {
        return;
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_claimed_task(task))).is_err() {
        let _ = update_terminal(&library, &job_id, "Failed", "缩略图工作线程发生异常");
    }
    let _ = acknowledge_cancellation(&library, &job_id);
}

fn run_claimed_task(task: ThumbnailTask) {
    let ThumbnailTask {
        library, job_id, ..
    } = task;
    let result = asset_id_for_job(&library, &job_id).and_then(|asset_id| {
        let asset = library.inspect_asset(&asset_id)?;
        let extension = std::path::Path::new(&asset.primary_source)
            .extension()
            .and_then(|extension| extension.to_str());
        if !extension
            .is_some_and(|extension| asset.asset_type.supports_thumbnail_extension(extension))
        {
            return Err(CoreError::ThumbnailRender(
                "当前队列渲染器支持 PMX 模型/场景、PMD 场景，以及配置了 Motion Preview Model 的 VMD/VPD 动作".to_owned(),
            ));
        }
        library.create_card_with_thumbnail_progress(&asset_id, &mut |status, progress| {
            update_progress(&library, &job_id, status, progress).unwrap_or(false)
                && !is_cancelled(&library, &job_id).unwrap_or(true)
        })
    });
    match result {
        Ok(_) => {
            if is_cancelled(&library, &job_id).unwrap_or(true) {
                return;
            }
            let _ = update_progress(&library, &job_id, "Completed", 1.0);
        }
        Err(error) => {
            if !is_cancelled(&library, &job_id).unwrap_or(true) {
                let _ = update_terminal(&library, &job_id, "Failed", &error.to_string());
            }
        }
    }
}

fn update_progress(
    library: &Library,
    job_id: &str,
    status: &str,
    progress: f64,
) -> CoreResult<bool> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE jobs SET status=?1,progress=?2,updated_at=?3
         ,claim_owner=CASE WHEN ?1='Completed' THEN NULL ELSE claim_owner END
         WHERE id=?4 AND kind='thumbnail' AND status NOT IN ('Completed','Failed','Cancelled','Cancelling')",
        params![status, progress.clamp(0.0, 1.0), now, job_id],
    )?;
    Ok(changed > 0)
}

fn claim_job(library: &Library, job_id: &str) -> CoreResult<bool> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE jobs SET status='Parsing',progress=0.02,updated_at=?1,claim_owner=?3
         WHERE id=?2 AND kind='thumbnail' AND status='Pending'",
        params![now, job_id, i64::from(std::process::id())],
    )?;
    Ok(changed > 0)
}

fn update_terminal(library: &Library, job_id: &str, status: &str, message: &str) -> CoreResult<()> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    connection.execute(
        "UPDATE jobs SET status=?1,error_json=json_object('message',?2),updated_at=?3,claim_owner=NULL
         WHERE id=?4 AND status NOT IN ('Completed','Failed','Cancelled','Cancelling')",
        params![status, message, now, job_id],
    )?;
    Ok(())
}

fn acknowledge_cancellation(library: &Library, job_id: &str) -> CoreResult<()> {
    library.connection()?.execute(
        "UPDATE jobs SET status='Cancelled',claim_owner=NULL,updated_at=?2
         WHERE id=?1 AND status='Cancelling' AND claim_owner=?3",
        params![job_id, Utc::now().to_rfc3339(), i64::from(std::process::id())],
    )?;
    Ok(())
}

fn is_cancelled(library: &Library, job_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT status IN ('Cancelled','Cancelling') FROM jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| CoreError::JobNotFound(job_id.to_owned()))
}

fn asset_id_for_job(library: &Library, job_id: &str) -> CoreResult<String> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT asset_id FROM jobs WHERE id=?1 AND kind='thumbnail'",
            [job_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| CoreError::JobNotFound(job_id.to_owned()))
}

fn get_job(library: &Library, job_id: &str) -> CoreResult<Value> {
    let connection = library.connection()?;
    connection.query_row(
        "SELECT id,asset_id,kind,priority,status,progress,error_json,created_at,updated_at
         FROM jobs WHERE id=?1",
        [job_id],
        |row| {
            let error_json: Option<String> = row.get(6)?;
            Ok(serde_json::json!({
                "id":row.get::<_,String>(0)?, "asset_id":row.get::<_,Option<String>>(1)?,
                "kind":row.get::<_,String>(2)?, "priority":row.get::<_,i64>(3)?,
                "status":row.get::<_,String>(4)?, "progress":row.get::<_,f64>(5)?,
                "error":error_json.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
                "created_at":row.get::<_,String>(7)?, "updated_at":row.get::<_,String>(8)?
            }))
        },
    ).optional()?
        .ok_or_else(|| CoreError::JobNotFound(job_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Barrier;

    fn seed(library: &Library) {
        library.connection().unwrap().execute_batch(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at)
             VALUES ('r','model','/models','/models','Models','now');
             INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,created_at,updated_at,last_seen_at)
             VALUES ('a','r','model','A','/models/a.pmx','/models','now','now','now'),
                    ('b','r','model','B','/models/b.pmx','/models','now','now','now'),
                    ('c','r','model','C','/models/c.pmx','/models','now','now','now');"
        ).unwrap();
    }

    #[test]
    fn concurrent_clients_reserve_one_thumbnail() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-queue-{}", Uuid::new_v4()));
        let path = directory.join("library.sqlite3");
        let library = Library::open(&path).unwrap();
        seed(&library);
        let clients = (0..8).map(|_| Library::open(&path).unwrap()).collect::<Vec<_>>();
        let barrier = Arc::new(Barrier::new(clients.len()));
        let workers = clients.into_iter().map(|client| {
            let barrier = barrier.clone();
            thread::spawn(move || { barrier.wait(); reserve_thumbnail(&client, "a", 0).unwrap() })
        }).collect::<Vec<_>>();
        let reservations = workers.into_iter().map(|worker| worker.join().unwrap()).collect::<Vec<_>>();
        assert_eq!(reservations.iter().filter(|(_, created)| *created).count(), 1);
        assert_eq!(reservations.iter().map(|(id, _)| id).collect::<HashSet<_>>().len(), 1);
        drop(library);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn running_cancellation_holds_reservation_until_worker_acknowledges() {
        let library = Library::in_memory().unwrap();
        seed(&library);
        let (id, _) = reserve_thumbnail(&library, "a", 0).unwrap();
        assert!(claim_job(&library, &id).unwrap());
        assert!(cancel_job(&library, &id).unwrap());
        assert_eq!(get_job(&library, &id).unwrap()["status"], "Cancelling");
        run_task(new_task(library.clone(), id.clone(), 0));
        assert_eq!(get_job(&library, &id).unwrap()["status"], "Cancelling");
        assert!(!update_progress(&library, &id, "Completed", 1.0).unwrap());
        assert_eq!(reserve_thumbnail(&library, "a", 0).unwrap(), (id.clone(), false));
        assert!(!reserve_retry(&library, &id).unwrap().2);
        acknowledge_cancellation(&library, &id).unwrap();
        assert_eq!(get_job(&library, &id).unwrap()["status"], "Cancelled");
        assert!(reserve_retry(&library, &id).unwrap().2);
        assert_eq!(get_job(&library, &id).unwrap()["status"], "Pending");
    }

    #[test]
    fn pending_cancellation_never_starts_a_worker() {
        let library = Library::in_memory().unwrap();
        seed(&library);
        let (id, _) = reserve_thumbnail(&library, "a", 0).unwrap();
        assert!(cancel_job(&library, &id).unwrap());
        assert!(!claim_job(&library, &id).unwrap());
        assert_eq!(get_job(&library, &id).unwrap()["status"], "Cancelled");
    }

    #[test]
    fn retry_reuses_another_active_job_for_the_same_asset() {
        let library = Library::in_memory().unwrap();
        seed(&library);
        let (old, _) = reserve_thumbnail(&library, "a", 0).unwrap();
        update_terminal(&library, &old, "Failed", "fixture failure").unwrap();
        let (active, _) = reserve_thumbnail(&library, "a", 1).unwrap();
        let (id, _, queued) = reserve_retry(&library, &old).unwrap();
        assert_eq!(id, active);
        assert!(!queued);
        assert_eq!(get_job(&library, &old).unwrap()["status"], "Failed");
    }

    #[test]
    fn invalid_batch_rolls_back_all_new_reservations() {
        let library = Library::in_memory().unwrap();
        seed(&library);
        assert!(reserve_thumbnails(&library, &["a".to_owned(), "missing".to_owned()], 0).is_err());
        assert!(active_job(&library.connection().unwrap(), "a").unwrap().is_none());
        let reserved = reserve_thumbnails(&library, &["a".to_owned(), "a".to_owned(), "b".to_owned()], 0).unwrap();
        assert_eq!(reserved[0].0, reserved[1].0);
        assert_eq!(reserved.iter().filter(|(_, created)| *created).count(), 2);
    }

    #[test]
    fn restart_preserves_live_workers_and_recovers_dead_workers_immediately() {
        let library = Library::in_memory().unwrap();
        seed(&library);
        let (live, _) = reserve_thumbnail(&library, "a", 0).unwrap();
        let (dead, _) = reserve_thumbnail(&library, "b", 0).unwrap();
        let (cancelled, _) = reserve_thumbnail(&library, "c", 0).unwrap();
        let connection = library.connection().unwrap();
        connection.execute("UPDATE jobs SET status='Rendering',claim_owner=?2,updated_at='2000-01-01T00:00:00Z' WHERE id=?1",
            params![live, i64::from(std::process::id())]).unwrap();
        connection.execute("UPDATE jobs SET status='Rendering',claim_owner=2147483647 WHERE id=?1", [&dead]).unwrap();
        connection.execute("UPDATE jobs SET status='Cancelling',claim_owner=2147483647 WHERE id=?1", [&cancelled]).unwrap();
        recover_abandoned_jobs(&connection).unwrap();
        drop(connection);
        assert_eq!(get_job(&library, &live).unwrap()["status"], "Rendering");
        assert_eq!(get_job(&library, &dead).unwrap()["status"], "Pending");
        assert_eq!(get_job(&library, &cancelled).unwrap()["status"], "Cancelled");
    }
}
