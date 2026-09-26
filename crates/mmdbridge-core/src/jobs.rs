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
use rusqlite::{OptionalExtension, params};
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
        connection.execute(
            "UPDATE jobs SET status='Pending',progress=0,updated_at=?1
             WHERE kind='thumbnail' AND status IN ('Parsing','Rendering','Encoding')
               AND updated_at<?2",
            params![Utc::now().to_rfc3339(), (Utc::now() - chrono::Duration::minutes(10)).to_rfc3339()],
        )?;
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
    library.inspect_asset(asset_id)?;
    let existing_job = {
        let connection = library.connection()?;
        connection.query_row(
            "SELECT id FROM jobs WHERE asset_id=?1 AND kind='thumbnail'
             AND status IN ('Pending','Parsing','Rendering','Encoding')
             ORDER BY created_at DESC LIMIT 1",
            [asset_id], |row| row.get::<_, String>(0),
        ).optional()?
    };
    if let Some(job_id) = existing_job {
        return get_job(library, &job_id);
    }
    if crate::operations::is_asset_operation_active(library, asset_id)? {
        return Err(CoreError::ThumbnailQueue(
            "该资产正在执行文件操作，暂时不能生成缩略图".to_owned(),
        ));
    }
    let job_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    {
        let connection = library.connection()?;
        connection.execute(
            "INSERT INTO jobs(id,asset_id,kind,priority,status,progress,error_json,created_at,updated_at)
             VALUES (?1,?2,'thumbnail',?3,'Pending',0,NULL,?4,?4)",
            params![job_id, asset_id, priority, now],
        )?;
    }
    let task = new_task(library.clone(), job_id.clone(), i64::from(priority));
    if let Err(message) = worker_queue()?.try_enqueue(task) {
        update_terminal(library, &job_id, "Failed", &message)?;
        return Err(CoreError::ThumbnailQueue(message));
    }
    get_job(library, &job_id)
}

pub(crate) fn enqueue_thumbnails(
    library: &Library,
    asset_ids: &[String],
    priority: i32,
) -> CoreResult<Vec<Value>> {
    asset_ids
        .iter()
        .map(|asset_id| enqueue_thumbnail(library, asset_id, priority))
        .collect()
}

pub(crate) fn cancel_job(library: &Library, job_id: &str) -> CoreResult<bool> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE jobs SET status='Cancelled',updated_at=?1,
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
    let job: Option<(String, i64, String)> = {
        let connection = library.connection()?;
        connection
            .query_row(
                "SELECT asset_id,priority,status FROM jobs WHERE id=?1 AND kind='thumbnail'",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
    };
    let Some((_asset_id, priority, status)) = job else {
        return Err(CoreError::JobNotFound(job_id.to_owned()));
    };
    if crate::operations::is_asset_operation_active(library, &_asset_id)? {
        return Err(CoreError::ThumbnailQueue(
            "该资产正在执行文件操作，暂时不能重试缩略图任务".to_owned(),
        ));
    }
    if !matches!(status.as_str(), "Failed" | "Cancelled") {
        return Err(CoreError::ThumbnailQueue(format!(
            "只能重试 Failed 或 Cancelled 任务，当前状态为 {status}"
        )));
    }
    let now = Utc::now().to_rfc3339();
    {
        let connection = library.connection()?;
        connection.execute(
            "UPDATE jobs SET status='Pending',progress=0,error_json=NULL,updated_at=?1 WHERE id=?2",
            params![now, job_id],
        )?;
    }
    let task = new_task(library.clone(), job_id.to_owned(), priority);
    if let Err(message) = worker_queue()?.try_enqueue(task) {
        update_terminal(library, job_id, "Failed", &message)?;
        return Err(CoreError::ThumbnailQueue(message));
    }
    get_job(library, job_id)
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
    fn try_enqueue(&self, task: ThumbnailTask) -> Result<(), String> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "缩略图队列锁不可用".to_owned())?;
        pending.push(task);
        self.changed.notify_one();
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
        let job_id = task.job_id.clone();
        let library = task.library.clone();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_task(task))).is_err() {
            let _ = update_terminal(&library, &job_id, "Failed", "缩略图工作线程发生异常");
        }
    }
}

fn run_task(task: ThumbnailTask) {
    let ThumbnailTask {
        library, job_id, ..
    } = task;
    if !claim_job(&library, &job_id).unwrap_or(false) {
        return;
    }
    let result = asset_id_for_job(&library, &job_id).and_then(|asset_id| {
        let asset = library.inspect_asset(&asset_id)?;
        let extension = std::path::Path::new(&asset.primary_source)
            .extension()
            .and_then(|extension| extension.to_str());
        if !extension
            .is_some_and(|extension| asset.asset_type.supports_thumbnail_extension(extension))
        {
            return Err(CoreError::ThumbnailRender(
                "当前队列渲染器支持 PMX 模型/场景、PMD 和文本 X 场景，以及配置了 Motion Preview Model 的 VMD/VPD 动作".to_owned(),
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
         WHERE id=?4 AND kind='thumbnail' AND status NOT IN ('Completed','Failed','Cancelled')",
        params![status, progress.clamp(0.0, 1.0), now, job_id],
    )?;
    Ok(changed > 0)
}

fn claim_job(library: &Library, job_id: &str) -> CoreResult<bool> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE jobs SET status='Parsing',progress=0.02,updated_at=?1
         WHERE id=?2 AND kind='thumbnail' AND status='Pending'",
        params![now, job_id],
    )?;
    Ok(changed > 0)
}

fn update_terminal(library: &Library, job_id: &str, status: &str, message: &str) -> CoreResult<()> {
    let now = Utc::now().to_rfc3339();
    let connection = library.connection()?;
    connection.execute(
        "UPDATE jobs SET status=?1,error_json=json_object('message',?2),updated_at=?3
         WHERE id=?4 AND status NOT IN ('Completed','Failed','Cancelled')",
        params![status, message, now, job_id],
    )?;
    Ok(())
}

fn is_cancelled(library: &Library, job_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT status='Cancelled' FROM jobs WHERE id=?1",
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
