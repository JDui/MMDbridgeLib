use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use chrono::Utc;
use crate::model_io::{parse_pmx_model, read_source};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    CoreError, CoreResult, scanner,
    thumbnail::GeneratedThumbnail,
    thumbnail_concurrency::ThumbnailConcurrencySettings,
    types::{
        Asset, AssetCursor, AssetDirectory, AssetPage, AssetTag, AssetType, CardResult,
        CardValidation, DirectoryPage, FilterExpr, Root, SavedFilter, ScanReport, ScanState,
        TagMutation,
    },
};

#[derive(Clone)]
pub struct Library {
    connection: Arc<Mutex<Connection>>,
    database_path: Option<Arc<PathBuf>>,
    scan_lock: Arc<Mutex<()>>,
    visibility_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
pub struct LibraryOpenProgress {
    pub step: u8,
    pub phase: String,
    pub detail: String,
    pub completed: Option<usize>,
    pub total: Option<usize>,
}

impl LibraryOpenProgress {
    fn stage(step: u8, phase: &str, detail: impl Into<String>) -> Self {
        Self { step, phase: phase.to_owned(), detail: detail.into(), completed: None, total: None }
    }
}

impl Library {
    const SCHEMA_VERSION: i64 = 16;

    fn check_schema_version(connection: &Connection) -> CoreResult<i64> {
        let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > Self::SCHEMA_VERSION {
            return Err(CoreError::UnsupportedDatabaseVersion {
                found: version,
                supported: Self::SCHEMA_VERSION,
            });
        }
        Ok(version)
    }

    pub(crate) fn visibility_guard(&self) -> CoreResult<MutexGuard<'_, ()>> {
        self.visibility_lock
            .lock()
            .map_err(|_| CoreError::LockPoisoned)
    }

    pub const DATABASE_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
    pub const WAL_TARGET_BYTES: u64 = 32 * 1024 * 1024;

    pub fn portable_database_path() -> CoreResult<std::path::PathBuf> {
        let executable = std::env::current_exe()?;
        let directory = executable.parent().ok_or_else(|| {
            CoreError::Io(std::io::Error::other("无法确定程序所在目录"))
        })?;
        Ok(directory.join("data").join("library.sqlite3"))
    }

    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        Self::open_with_progress(path, &mut |_| {})
    }

    pub fn open_with_progress(
        path: impl AsRef<Path>,
        progress: &mut impl FnMut(LibraryOpenProgress),
    ) -> CoreResult<Self> {
        progress(LibraryOpenProgress::stage(1, "打开本地数据库", path.as_ref().display().to_string()));
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path.as_ref())?;
        Self::check_schema_version(&connection)?;
        let database_path = connection.path().filter(|path| !path.is_empty())
            .map(|_| std::fs::canonicalize(path.as_ref()).map(Arc::new)).transpose()?;
        connection.busy_timeout(Duration::from_secs(5))?;
        progress(LibraryOpenProgress::stage(2, "准备数据库读写", "启用外键、WAL 日志；若其他程序占用写入锁，最多等待 5 秒"));
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        progress(LibraryOpenProgress::stage(3, "检查数据库容量", "读取页数并配置数据库与日志大小限制"));
        let page_size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        let max_pages = (Self::DATABASE_LIMIT_BYTES as i64) / page_size;
        let current_pages: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
        if current_pages > max_pages {
            return Err(CoreError::StorageLimit(format!(
                "数据库已超过 512 MiB 上限：{} 页", current_pages
            )));
        }
        connection.pragma_update(None, "max_page_count", max_pages)?;
        connection.pragma_update(None, "wal_autocheckpoint", 512)?;
        connection.pragma_update(None, "journal_size_limit", Self::WAL_TARGET_BYTES as i64)?;
        let library = Self {
            connection: Arc::new(Mutex::new(connection)),
            database_path,
            scan_lock: Arc::new(Mutex::new(())),
            visibility_lock: Arc::new(Mutex::new(())),
        };
        progress(LibraryOpenProgress::stage(4, "检查与升级数据库结构", "读取已有数据库版本"));
        library.initialize_schema_with_progress(progress)?;
        progress(LibraryOpenProgress::stage(5, "读取缩略图设置", "读取解析、渲染、编码并发设置"));
        library.thumbnail_concurrency()?;
        progress(LibraryOpenProgress::stage(6, "检查旧版缩略图", "读取渲染版本；此阶段不扫描模型目录或生成缩略图"));
        library.mark_outdated_thumbnails_with_progress(progress)?;
        Ok(library)
    }

    pub fn in_memory() -> CoreResult<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let library = Self {
            connection: Arc::new(Mutex::new(connection)),
            database_path: None,
            scan_lock: Arc::new(Mutex::new(())),
            visibility_lock: Arc::new(Mutex::new(())),
        };
        library.initialize_schema()?;
        library.thumbnail_concurrency()?;
        Ok(library)
    }

    pub fn storage_info(&self) -> CoreResult<serde_json::Value> {
        let path = self.database_path.as_deref();
        let bytes = |path: &Path| std::fs::metadata(path).map(|item| item.len()).unwrap_or(0);
        let wal = path.map(|path| {
            let mut name = path.as_os_str().to_os_string();
            name.push("-wal");
            PathBuf::from(name)
        });
        Ok(serde_json::json!({
            "path": path.map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|| ":memory:".to_owned()),
            "databaseBytes": path.map_or(0, |path| bytes(path)),
            "walBytes": wal.as_deref().map_or(0, bytes),
            "databaseLimitBytes": Self::DATABASE_LIMIT_BYTES,
            "walTargetBytes": Self::WAL_TARGET_BYTES,
        }))
    }

    pub(crate) fn same_database(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.connection, &other.connection)
            || matches!((&self.database_path, &other.database_path), (Some(left), Some(right)) if left == right)
    }

    pub fn compact_storage(&self) -> CoreResult<serde_json::Value> {
        let _scan_guard = self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
        let connection = self.connection()?;
        let active_scans: i64 = connection.query_row(
            "SELECT COUNT(*) FROM scan_state WHERE status IN ('Pending','Pausing','Discovering','Indexing','Verifying','Relations','Cancelling')",
            [], |row| row.get(0),
        )?;
        let active_cards: i64 = connection.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('Pending','Parsing','Rendering','Encoding','Cancelling')",
            [], |row| row.get(0),
        )?;
        if active_scans + active_cards > 0 {
            return Err(CoreError::StorageLimit("请先暂停扫描并等待资源卡任务完成，再整理数据库".to_owned()));
        }
        connection.execute(
            "DELETE FROM jobs WHERE status IN ('Completed','Failed','Cancelled') AND id NOT IN
             (SELECT id FROM jobs ORDER BY updated_at DESC LIMIT 1000)", [],
        )?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA optimize;")?;
        drop(connection);
        self.storage_info()
    }

    fn initialize_schema(&self) -> CoreResult<()> {
        self.initialize_schema_with_progress(&mut |_| {})
    }

    fn initialize_schema_with_progress(&self, progress: &mut impl FnMut(LibraryOpenProgress)) -> CoreResult<()> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let schema_version = Self::check_schema_version(&connection)?;
        if schema_version == Self::SCHEMA_VERSION {
            progress(LibraryOpenProgress::stage(4, "检查与升级数据库结构", format!("数据库版本 {schema_version}，结构已是最新，无需升级")));
            return Ok(());
        }
        progress(LibraryOpenProgress::stage(4, "检查与升级数据库结构", format!("当前版本 {schema_version}，准备基础表与目录计数")));
        connection.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS roots (
                 id TEXT PRIMARY KEY,
                 asset_type TEXT NOT NULL CHECK(asset_type IN ('model','motion','scene')),
                 path TEXT NOT NULL, path_key TEXT NOT NULL, display_name TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, scan_recursive INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL, last_scan_at TEXT, scan_status TEXT NOT NULL DEFAULT 'NeverScanned',
                 UNIQUE(asset_type, path_key)
             );
             CREATE TABLE IF NOT EXISTS assets (
                 id TEXT PRIMARY KEY, root_id TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
                 asset_type TEXT NOT NULL, name TEXT NOT NULL, primary_source TEXT NOT NULL,
                 asset_directory TEXT NOT NULL, fingerprint TEXT NOT NULL DEFAULT '', statuses_json TEXT NOT NULL DEFAULT '[]',
                 retired_format INTEGER NOT NULL DEFAULT 0 CHECK(retired_format IN (0,1)),
                 visibility TEXT NOT NULL DEFAULT 'normal' CHECK(visibility IN ('normal','auxiliary')),
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_seen_at TEXT NOT NULL,
                 UNIQUE(root_id, primary_source)
             );
             CREATE INDEX IF NOT EXISTS idx_assets_type_name ON assets(asset_type, name COLLATE NOCASE);
             CREATE INDEX IF NOT EXISTS idx_assets_fingerprint ON assets(asset_type, fingerprint);
             CREATE INDEX IF NOT EXISTS idx_assets_root ON assets(root_id);
             CREATE INDEX IF NOT EXISTS idx_assets_directory ON assets(asset_directory COLLATE NOCASE);
             CREATE TABLE IF NOT EXISTS asset_counts (
                 root_id TEXT NOT NULL, asset_type TEXT NOT NULL, asset_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(root_id, asset_type)
             );
             CREATE TABLE IF NOT EXISTS asset_directory_counts (
                 root_id TEXT NOT NULL, directory TEXT NOT NULL, asset_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY(root_id, directory)
             );
             CREATE TRIGGER IF NOT EXISTS assets_count_insert AFTER INSERT ON assets BEGIN
                 INSERT INTO asset_counts(root_id,asset_type,asset_count) VALUES (NEW.root_id,NEW.asset_type,1)
                 ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+1;
                 INSERT INTO asset_directory_counts(root_id,directory,asset_count) VALUES (NEW.root_id,NEW.asset_directory,1)
                 ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+1;
             END;
             CREATE TRIGGER IF NOT EXISTS assets_count_delete AFTER DELETE ON assets BEGIN
                 UPDATE asset_counts SET asset_count=asset_count-1
                 WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type;
                 UPDATE asset_directory_counts SET asset_count=asset_count-1
                 WHERE root_id=OLD.root_id AND directory=OLD.asset_directory;
                 DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
                 DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
             END;
             CREATE TRIGGER IF NOT EXISTS assets_count_move AFTER UPDATE OF root_id,asset_type ON assets
             WHEN OLD.root_id!=NEW.root_id OR OLD.asset_type!=NEW.asset_type BEGIN
                 UPDATE asset_counts SET asset_count=asset_count-1
                 WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type;
                 INSERT INTO asset_counts(root_id,asset_type,asset_count) VALUES (NEW.root_id,NEW.asset_type,1)
                 ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+1;
                 DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
             END;
             CREATE TRIGGER IF NOT EXISTS assets_directory_count_move AFTER UPDATE OF root_id,asset_directory ON assets
             WHEN OLD.root_id!=NEW.root_id OR OLD.asset_directory!=NEW.asset_directory BEGIN
                 UPDATE asset_directory_counts SET asset_count=asset_count-1
                 WHERE root_id=OLD.root_id AND directory=OLD.asset_directory;
                 INSERT INTO asset_directory_counts(root_id,directory,asset_count) VALUES (NEW.root_id,NEW.asset_directory,1)
                 ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+1;
                 DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
             END;
             CREATE TRIGGER IF NOT EXISTS roots_count_delete AFTER DELETE ON roots BEGIN
                 DELETE FROM asset_counts WHERE root_id=OLD.id;
                 DELETE FROM asset_directory_counts WHERE root_id=OLD.id;
             END;
             CREATE TABLE IF NOT EXISTS asset_files (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 path TEXT NOT NULL, role TEXT NOT NULL, file_size INTEGER NOT NULL, modified_ns INTEGER NOT NULL,
                 UNIQUE(asset_id, path)
             );
             CREATE INDEX IF NOT EXISTS idx_asset_files_asset_role ON asset_files(asset_id, role);
             CREATE TABLE IF NOT EXISTS metadata (
                 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE, key TEXT NOT NULL,
                 value_json TEXT NOT NULL, PRIMARY KEY(asset_id, key)
             );
             CREATE TABLE IF NOT EXISTS tags (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, source TEXT NOT NULL DEFAULT 'user',
                 confidence REAL, created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS asset_tags (
                 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE, source TEXT NOT NULL,
                 confidence REAL, PRIMARY KEY(asset_id, tag_id)
             );
             CREATE TABLE IF NOT EXISTS asset_tag_overrides (
                 asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 normalized_name TEXT NOT NULL, updated_at TEXT NOT NULL,
                 PRIMARY KEY(asset_id, normalized_name)
             );
             CREATE TABLE IF NOT EXISTS favorites (
                 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS relations (
                 id TEXT PRIMARY KEY, relation_type TEXT NOT NULL,
                 source_asset TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 target_asset TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 confidence REAL NOT NULL, reason_json TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS versions (
                 family_id TEXT NOT NULL, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 version_label TEXT NOT NULL, confidence REAL NOT NULL, reason_json TEXT NOT NULL,
                 PRIMARY KEY(family_id, asset_id)
             );
             CREATE TABLE IF NOT EXISTS cards (
                 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
                 card_path TEXT NOT NULL, status TEXT NOT NULL, manifest_json TEXT, last_checked_at TEXT NOT NULL,
                 file_size INTEGER, modified_ns INTEGER, manifest_revision TEXT, renderer_revision TEXT,
                 has_thumbnail INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS jobs (
                 id TEXT PRIMARY KEY, asset_id TEXT REFERENCES assets(id) ON DELETE CASCADE,
                 kind TEXT NOT NULL, priority INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL,
                 progress REAL NOT NULL DEFAULT 0, error_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS operation_journal (
                 id TEXT PRIMARY KEY, operation TEXT NOT NULL, status TEXT NOT NULL,
                 asset_ids_json TEXT NOT NULL, sources_json TEXT NOT NULL, destinations_json TEXT NOT NULL,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, result_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_operation_journal_updated_at ON operation_journal(updated_at DESC);
             CREATE TABLE IF NOT EXISTS scan_state (
                 root_id TEXT PRIMARY KEY REFERENCES roots(id) ON DELETE CASCADE,
                 status TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0, error_json TEXT, updated_at TEXT NOT NULL,
                 files_seen INTEGER NOT NULL DEFAULT 0, files_processed INTEGER NOT NULL DEFAULT 0,
                 queue_order INTEGER NOT NULL DEFAULT 0,
                 full_check INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS saved_filters (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                 expression_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             ",
        )?;
        // Older schemas gain retired_format in the later v11 migration.
        let active_asset_filter = match schema_version {
            12.. => "retired_format=0 AND visibility='normal' AND ",
            11 => "retired_format=0 AND ",
            _ => "",
        };
        connection.execute_batch(&format!(
            "INSERT OR IGNORE INTO asset_counts(root_id,asset_type,asset_count)
                 SELECT root_id,asset_type,COUNT(*) FROM assets
                 WHERE {active_asset_filter}NOT EXISTS(SELECT 1 FROM asset_counts) GROUP BY root_id,asset_type;
             INSERT OR IGNORE INTO asset_directory_counts(root_id,directory,asset_count)
                 SELECT root_id,asset_directory,COUNT(*) FROM assets
                 WHERE {active_asset_filter}NOT EXISTS(SELECT 1 FROM asset_directory_counts) GROUP BY root_id,asset_directory;"
        ))?;
        if schema_version < 10 {
            connection.execute_batch(
                "UPDATE roots SET scan_status='Paused'
                   WHERE id IN (SELECT root_id FROM scan_state WHERE status='Duplicates');
                 UPDATE scan_state SET status='Paused' WHERE status='Duplicates';
                 DROP TABLE IF EXISTS duplicates;
                 PRAGMA user_version = 10;",
            )?;
        }
        connection.execute_batch("COMMIT;")?;
        let columns = connection
            .prepare("PRAGMA table_info(scan_state)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<HashSet<_>, _>>()?;
        if !columns.contains("files_seen") {
            connection.execute("ALTER TABLE scan_state ADD COLUMN files_seen INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !columns.contains("files_processed") {
            connection.execute("ALTER TABLE scan_state ADD COLUMN files_processed INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !columns.contains("queue_order") {
            connection.execute("ALTER TABLE scan_state ADD COLUMN queue_order INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !columns.contains("full_check") {
            connection.execute("ALTER TABLE scan_state ADD COLUMN full_check INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if schema_version < 11 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：停用 X 格式", "保留源文件，更新可见性与目录计数"));
            let asset_columns = connection
                .prepare("PRAGMA table_info(assets)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<HashSet<_>, _>>()?;
            let add_retired_format = if asset_columns.contains("retired_format") {
                ""
            } else {
                "ALTER TABLE assets ADD COLUMN retired_format INTEGER NOT NULL DEFAULT 0 CHECK(retired_format IN (0,1));"
            };
            let migration = format!(
                "BEGIN;
                 {add_retired_format}
                 UPDATE assets SET retired_format=1
                   WHERE asset_type='scene' AND lower(primary_source) LIKE '%.x';
                 UPDATE jobs SET status='Failed',progress=0,
                   error_json=json_object('message','格式已不再支持：X'),
                   updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   WHERE kind='thumbnail' AND status IN ('Pending','Parsing','Rendering','Encoding')
                     AND asset_id IN (SELECT id FROM assets WHERE retired_format=1);
                 DELETE FROM asset_counts;
                 INSERT INTO asset_counts(root_id,asset_type,asset_count)
                   SELECT root_id,asset_type,COUNT(*) FROM assets WHERE retired_format=0 GROUP BY root_id,asset_type;
                 DELETE FROM asset_directory_counts;
                 INSERT INTO asset_directory_counts(root_id,directory,asset_count)
                   SELECT root_id,asset_directory,COUNT(*) FROM assets WHERE retired_format=0 GROUP BY root_id,asset_directory;
                 DROP TRIGGER IF EXISTS assets_count_insert;
                 DROP TRIGGER IF EXISTS assets_count_delete;
                 DROP TRIGGER IF EXISTS assets_count_move;
                 DROP TRIGGER IF EXISTS assets_directory_count_move;
                 CREATE TRIGGER assets_count_insert AFTER INSERT ON assets WHEN NEW.retired_format=0 BEGIN
                   INSERT INTO asset_counts(root_id,asset_type,asset_count) VALUES (NEW.root_id,NEW.asset_type,1)
                     ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+1;
                   INSERT INTO asset_directory_counts(root_id,directory,asset_count) VALUES (NEW.root_id,NEW.asset_directory,1)
                     ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+1;
                 END;
                 CREATE TRIGGER assets_count_delete AFTER DELETE ON assets WHEN OLD.retired_format=0 BEGIN
                   UPDATE asset_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type;
                   UPDATE asset_directory_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND directory=OLD.asset_directory;
                   DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
                   DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
                 END;
                 CREATE TRIGGER assets_count_move AFTER UPDATE OF root_id,asset_type,retired_format ON assets
                 WHEN OLD.root_id!=NEW.root_id OR OLD.asset_type!=NEW.asset_type OR OLD.retired_format!=NEW.retired_format BEGIN
                   UPDATE asset_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND OLD.retired_format=0;
                   INSERT INTO asset_counts(root_id,asset_type,asset_count)
                     VALUES (NEW.root_id,NEW.asset_type,CASE WHEN NEW.retired_format=0 THEN 1 ELSE 0 END)
                     ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+excluded.asset_count;
                   DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
                   DELETE FROM asset_counts WHERE root_id=NEW.root_id AND asset_type=NEW.asset_type AND asset_count<=0;
                 END;
                 CREATE TRIGGER assets_directory_count_move AFTER UPDATE OF root_id,asset_directory,retired_format ON assets
                 WHEN OLD.root_id!=NEW.root_id OR OLD.asset_directory!=NEW.asset_directory OR OLD.retired_format!=NEW.retired_format BEGIN
                   UPDATE asset_directory_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND OLD.retired_format=0;
                   INSERT INTO asset_directory_counts(root_id,directory,asset_count)
                     VALUES (NEW.root_id,NEW.asset_directory,CASE WHEN NEW.retired_format=0 THEN 1 ELSE 0 END)
                     ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+excluded.asset_count;
                   DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
                   DELETE FROM asset_directory_counts WHERE root_id=NEW.root_id AND directory=NEW.asset_directory AND asset_count<=0;
                 END;
                 PRAGMA user_version = 11;
                 COMMIT;"
            );
            connection.execute_batch(&migration)?;
        }
        if schema_version < 12 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：相机分类", "更新纯镜头资产的可见性与目录计数"));
            let asset_columns = connection
                .prepare("PRAGMA table_info(assets)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<HashSet<_>, _>>()?;
            let add_visibility = if asset_columns.contains("visibility") {
                ""
            } else {
                "ALTER TABLE assets ADD COLUMN visibility TEXT NOT NULL DEFAULT 'normal' CHECK(visibility IN ('normal','auxiliary'));"
            };
            let migration = format!(
                "BEGIN;
                 {add_visibility}
                 UPDATE assets SET visibility='auxiliary'
                   WHERE asset_type='motion' AND lower(primary_source) LIKE '%.vmd'
                     AND instr(statuses_json,'ParseFailed')=0
                     AND EXISTS(
                       SELECT 1 FROM metadata m
                       WHERE m.asset_id=assets.id AND m.key='parsed'
                         AND json_valid(m.value_json)
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.file_type')='vmd'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.bones')='integer'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.morphs')='integer'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.cameras')='integer'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.lights')='integer'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.selfShadows')='integer'
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.properties')='integer'
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.cameras')>0
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.bones')=0
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.morphs')=0
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.lights')=0
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.selfShadows')=0
                         AND json_extract(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.key_counts.properties')=0
                         AND json_type(CASE WHEN json_valid(m.value_json) THEN m.value_json ELSE '{{}}' END,'$.error') IS NULL
                     );
                 UPDATE metadata SET value_json=json_set(
                   value_json,'$.is_camera_only',
                   json((SELECT CASE WHEN visibility='auxiliary' THEN 'true' ELSE 'false' END
                         FROM assets WHERE assets.id=metadata.asset_id)))
                   WHERE key='parsed' AND json_valid(value_json)
                     AND json_extract(CASE WHEN json_valid(value_json) THEN value_json ELSE '{{}}' END,'$.file_type')='vmd'
                     AND asset_id IN (SELECT id FROM assets WHERE asset_type='motion' AND lower(primary_source) LIKE '%.vmd');
                 UPDATE jobs SET status='Cancelled',progress=0,
                   error_json=json_object('message','纯 Camera 不再进入普通资源卡队列'),
                   updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
                   WHERE kind='thumbnail' AND status IN ('Pending','Parsing','Rendering','Encoding')
                     AND asset_id IN (SELECT id FROM assets WHERE visibility='auxiliary');
                 DELETE FROM asset_counts;
                 INSERT INTO asset_counts(root_id,asset_type,asset_count)
                   SELECT root_id,asset_type,COUNT(*) FROM assets
                   WHERE retired_format=0 AND visibility='normal' GROUP BY root_id,asset_type;
                 DELETE FROM asset_directory_counts;
                 INSERT INTO asset_directory_counts(root_id,directory,asset_count)
                   SELECT root_id,asset_directory,COUNT(*) FROM assets
                   WHERE retired_format=0 AND visibility='normal' GROUP BY root_id,asset_directory;
                 DROP TRIGGER IF EXISTS assets_count_insert;
                 DROP TRIGGER IF EXISTS assets_count_delete;
                 DROP TRIGGER IF EXISTS assets_count_move;
                 DROP TRIGGER IF EXISTS assets_directory_count_move;
                 CREATE TRIGGER assets_count_insert AFTER INSERT ON assets
                   WHEN NEW.retired_format=0 AND NEW.visibility='normal' BEGIN
                   INSERT INTO asset_counts(root_id,asset_type,asset_count) VALUES (NEW.root_id,NEW.asset_type,1)
                     ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+1;
                   INSERT INTO asset_directory_counts(root_id,directory,asset_count) VALUES (NEW.root_id,NEW.asset_directory,1)
                     ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+1;
                 END;
                 CREATE TRIGGER assets_count_delete AFTER DELETE ON assets
                   WHEN OLD.retired_format=0 AND OLD.visibility='normal' BEGIN
                   UPDATE asset_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type;
                   UPDATE asset_directory_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND directory=OLD.asset_directory;
                   DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
                   DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
                 END;
                 CREATE TRIGGER assets_count_move AFTER UPDATE OF root_id,asset_type,retired_format,visibility ON assets
                   WHEN OLD.root_id!=NEW.root_id OR OLD.asset_type!=NEW.asset_type
                     OR OLD.retired_format!=NEW.retired_format OR OLD.visibility!=NEW.visibility BEGIN
                   UPDATE asset_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type
                       AND OLD.retired_format=0 AND OLD.visibility='normal';
                   INSERT INTO asset_counts(root_id,asset_type,asset_count)
                     VALUES (NEW.root_id,NEW.asset_type,
                       CASE WHEN NEW.retired_format=0 AND NEW.visibility='normal' THEN 1 ELSE 0 END)
                     ON CONFLICT(root_id,asset_type) DO UPDATE SET asset_count=asset_count+excluded.asset_count;
                   DELETE FROM asset_counts WHERE root_id=OLD.root_id AND asset_type=OLD.asset_type AND asset_count<=0;
                   DELETE FROM asset_counts WHERE root_id=NEW.root_id AND asset_type=NEW.asset_type AND asset_count<=0;
                 END;
                 CREATE TRIGGER assets_directory_count_move AFTER UPDATE OF root_id,asset_directory,retired_format,visibility ON assets
                   WHEN OLD.root_id!=NEW.root_id OR OLD.asset_directory!=NEW.asset_directory
                     OR OLD.retired_format!=NEW.retired_format OR OLD.visibility!=NEW.visibility BEGIN
                   UPDATE asset_directory_counts SET asset_count=asset_count-1
                     WHERE root_id=OLD.root_id AND directory=OLD.asset_directory
                       AND OLD.retired_format=0 AND OLD.visibility='normal';
                   INSERT INTO asset_directory_counts(root_id,directory,asset_count)
                     VALUES (NEW.root_id,NEW.asset_directory,
                       CASE WHEN NEW.retired_format=0 AND NEW.visibility='normal' THEN 1 ELSE 0 END)
                     ON CONFLICT(root_id,directory) DO UPDATE SET asset_count=asset_count+excluded.asset_count;
                   DELETE FROM asset_directory_counts WHERE root_id=OLD.root_id AND directory=OLD.asset_directory AND asset_count<=0;
                   DELETE FROM asset_directory_counts WHERE root_id=NEW.root_id AND directory=NEW.asset_directory AND asset_count<=0;
                 END;
                 PRAGMA user_version = 12;
                 COMMIT;"
            );
            connection.execute_batch(&migration)?;
        }
        if schema_version < 13 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：缩略图缓存", "补齐缓存版本、大小与修改时间字段"));
            let card_columns = connection
                .prepare("PRAGMA table_info(cards)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<HashSet<_>, _>>()?;
            let mut migration = String::from("BEGIN;");
            for (name, declaration) in [
                ("file_size", "INTEGER"),
                ("modified_ns", "INTEGER"),
                ("manifest_revision", "TEXT"),
                ("renderer_revision", "TEXT"),
                ("has_thumbnail", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                if !card_columns.contains(name) {
                    migration.push_str(&format!("ALTER TABLE cards ADD COLUMN {name} {declaration};"));
                }
            }
            migration.push_str(
                "UPDATE cards SET has_thumbnail=0,file_size=NULL,modified_ns=NULL,
                   manifest_revision=NULL,renderer_revision=NULL;",
            );
            migration.push_str("PRAGMA user_version = 13; COMMIT;");
            connection.execute_batch(&migration)?;
        }
        if schema_version < 14 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：增量索引", "补齐文件路径索引与扫描恢复字段"));
            let scan_columns = connection
                .prepare("PRAGMA table_info(scan_state)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<HashSet<_>, _>>()?;
            let file_columns = connection
                .prepare("PRAGMA table_info(asset_files)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<HashSet<_>, _>>()?;
            let add_generation = if scan_columns.contains("dirty_generation") {
                ""
            } else {
                "ALTER TABLE scan_state ADD COLUMN dirty_generation INTEGER NOT NULL DEFAULT 0;"
            };
            let add_claim_owner = if scan_columns.contains("claim_owner") {
                ""
            } else {
                "ALTER TABLE scan_state ADD COLUMN claim_owner INTEGER;"
            };
            let add_path_key = if file_columns.contains("path_key") {
                ""
            } else {
                "ALTER TABLE asset_files ADD COLUMN path_key TEXT NOT NULL DEFAULT '';"
            };
            connection.execute_batch(&format!(
                "BEGIN;
                 {add_generation}
                 {add_claim_owner}
                 {add_path_key}
                 CREATE TABLE IF NOT EXISTS scan_changes (
                   root_id TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
                   path_key TEXT NOT NULL, path TEXT NOT NULL,
                   scope TEXT NOT NULL CHECK(scope IN ('file','subtree','root')),
                   generation INTEGER NOT NULL, updated_at TEXT NOT NULL,
                   PRIMARY KEY(root_id,path_key)
                 );
                 CREATE INDEX IF NOT EXISTS idx_scan_changes_generation
                   ON scan_changes(root_id,generation);
                 COMMIT;"
            ))?;

            let files = {
                let mut statement = connection.prepare(
                    "SELECT id,path FROM asset_files WHERE path_key=''",
                )?;
                statement
                    .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if !files.is_empty() {
                let transaction = connection.unchecked_transaction()?;
                let total = files.len();
                for (index, (id, path)) in files.into_iter().enumerate() {
                    transaction.execute(
                        "UPDATE asset_files SET path_key=?2 WHERE id=?1",
                        params![id, scanner::scan_path_key(Path::new(&path))],
                    )?;
                    if index % 100 == 0 || index + 1 == total {
                        progress(LibraryOpenProgress { step: 4, phase: "升级数据库：文件路径索引".to_owned(),
                            detail: "规范化已有文件路径，不读取源模型内容".to_owned(), completed: Some(index + 1), total: Some(total) });
                    }
                }
                transaction.commit()?;
            }
            connection.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_asset_files_path_key_asset
                   ON asset_files(path_key,asset_id);
                 BEGIN;
                 UPDATE scan_state SET dirty_generation=dirty_generation+1
                 WHERE status IN ('Pending','Paused','Failed','Pausing','Discovering','Indexing','Verifying','Relations');
                 INSERT OR IGNORE INTO scan_changes(root_id,path_key,path,scope,generation,updated_at)
                   SELECT root_id,'','root','root',MAX(1,dirty_generation),updated_at
                   FROM scan_state
                   WHERE status IN ('Pending','Paused','Failed','Pausing','Discovering','Indexing','Verifying','Relations');
                 PRAGMA user_version = 14;
                 COMMIT;",
            )?;
        }
        if schema_version < 15 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：资产状态", "清理旧版复核标记，保留解析失败状态"));
            connection.execute_batch(
                "BEGIN;
                 UPDATE assets SET statuses_json = CASE
                   WHEN json_valid(statuses_json) THEN
                     COALESCE(NULLIF((SELECT json_group_array(value)
                       FROM json_each(assets.statuses_json) WHERE value <> 'NeedsReview'), '[]'), '[\"Ready\"]')
                   ELSE '[\"Ready\"]' END
                 WHERE instr(statuses_json,'NeedsReview')>0;
                 UPDATE metadata SET value_json=json_remove(value_json,
                   '$.candidate_reason','$.card_identity_ambiguous')
                 WHERE key='parsed' AND json_valid(value_json)
                   AND (instr(value_json,'candidate_reason')>0
                     OR instr(value_json,'card_identity_ambiguous')>0);
                 PRAGMA user_version = 15;
                 COMMIT;",
            )?;
        }
        if schema_version < 16 {
            progress(LibraryOpenProgress::stage(4, "升级数据库：任务恢复", "记录缩略图工作进程，保留取消中的占用状态"));
            let transaction = connection.unchecked_transaction()?;
            let has_owner = transaction.prepare("PRAGMA table_info(jobs)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?.iter().any(|name| name == "claim_owner");
            if !has_owner { transaction.execute("ALTER TABLE jobs ADD COLUMN claim_owner INTEGER", [])?; }
            transaction.execute("CREATE INDEX IF NOT EXISTS idx_jobs_asset_status ON jobs(asset_id,kind,status)", [])?;
            transaction.pragma_update(None, "user_version", Self::SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        Ok(())
    }

    pub fn mark_outdated_thumbnails(&self) -> CoreResult<usize> {
        self.mark_outdated_thumbnails_with_progress(&mut |_| {})
    }

    fn mark_outdated_thumbnails_with_progress(&self, progress: &mut impl FnMut(LibraryOpenProgress)) -> CoreResult<usize> {
        let revisions = [AssetType::Model, AssetType::Motion, AssetType::Scene]
            .into_iter().map(|kind| Ok((kind, crate::cards::expected_renderer_revision(self, kind)?)))
            .collect::<CoreResult<Vec<_>>>()?;
        let mut connection = self.connection()?;
        let total = connection.query_row("SELECT COUNT(*) FROM cards", [], |row| row.get::<_, i64>(0))? as usize;
        let mut outdated = Vec::new();
        progress(LibraryOpenProgress { step: 6, phase: "检查缩略图版本".to_owned(), detail: "只读取缓存版本，不重新渲染".to_owned(), completed: Some(0), total: Some(total) });
        {
            // One streaming pass, instead of three scans over large card manifests.
            let mut statement = connection.prepare(
                "SELECT c.asset_id,c.status,c.renderer_revision,a.asset_type,a.retired_format,a.visibility
                 FROM cards c JOIN assets a ON a.id=c.asset_id")?;
            let mut rows = statement.query([])?;
            let mut checked = 0;
            while let Some(row) = rows.next()? {
                let status: String = row.get(1)?;
                let revision: Option<String> = row.get(2)?;
                let kind: String = row.get(3)?;
                let retired: bool = row.get(4)?;
                let visibility: String = row.get(5)?;
                if status == "CardValid" && !retired && visibility == "normal" {
                    if let Some((_, expected)) = revisions.iter().find(|(asset_type, _)| asset_type.as_str() == kind) {
                        if revision.as_deref() != Some(expected.as_str()) {
                            outdated.push((row.get::<_, String>(0)?, expected.clone()));
                        }
                    }
                }
                checked += 1;
                if checked == 1 || checked % 100 == 0 || checked == total {
                    progress(LibraryOpenProgress { step: 6, phase: "检查缩略图版本".to_owned(),
                        detail: format!("已发现 {} 张旧版缩略图；不会自动重生成", outdated.len()), completed: Some(checked), total: Some(total) });
                }
            }
        }
        let mut changed = 0;
        let total = outdated.len();
        progress(LibraryOpenProgress { step: 6, phase: "标记旧版缩略图".to_owned(), detail: "只更新数据库状态，保留源素材与预览文件".to_owned(), completed: Some(0), total: Some(total) });
        for (batch_index, batch) in outdated.chunks(100).enumerate() {
            let transaction = connection.transaction()?;
            for (asset_id, revision) in batch {
                changed += transaction.execute(
                    "UPDATE cards SET status='CardStale' WHERE asset_id=?1 AND status='CardValid'
                     AND (renderer_revision IS NULL OR renderer_revision<>?2)
                     AND EXISTS(SELECT 1 FROM assets WHERE id=?1 AND retired_format=0 AND visibility='normal')",
                    params![asset_id, revision],
                )?;
            }
            transaction.commit()?;
            progress(LibraryOpenProgress { step: 6, phase: "标记旧版缩略图".to_owned(),
                detail: format!("已标记 {changed} 张；进入 Library 后可在设置中一键重生成"),
                completed: Some(((batch_index + 1) * 100).min(total)), total: Some(total) });
        }
        Ok(changed)
    }

    pub fn list_asset_page_with_filters(
        &self, asset_type: Option<AssetType>, query: Option<&str>, root_id: Option<&str>,
        favorite_only: bool, cursor: Option<&AssetCursor>, limit: usize, motion_format: Option<&str>,
        directory_path: Option<&str>, recursive_scope: bool, filter_id: Option<&str>, expression: Option<FilterExpr>,
    ) -> CoreResult<AssetPage> {
        let mut children = Vec::new();
        if let Some(id) = filter_id {
            let json: Option<String> = self.connection()?.query_row(
                "SELECT expression_json FROM saved_filters WHERE id=?1", [id], |row| row.get(0),
            ).optional()?;
            children.push(serde_json::from_str(&json.ok_or_else(|| CoreError::InvalidFilter(format!("saved filter was not found: {id}")))?)?);
        }
        if let Some(expression) = expression { children.push(expression); }
        let expression = (!children.is_empty()).then_some(FilterExpr::And { children });
        scanner::list_asset_page_filtered(self, asset_type, query, root_id, favorite_only, cursor,
            limit, motion_format, directory_path, recursive_scope, expression.as_ref())
    }

    pub fn list_tag_names(&self) -> CoreResult<Vec<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT t.name FROM tags t JOIN asset_tags at ON at.tag_id=t.id
             JOIN assets a ON a.id=at.asset_id WHERE a.retired_format=0 AND a.visibility='normal'
             ORDER BY t.name COLLATE NOCASE")?;
        statement.query_map([], |row| row.get(0))?.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn regenerate_thumbnail(&self, asset_id: &str) -> CoreResult<serde_json::Value> {
        self.ensure_asset_visible(asset_id)?;
        self.connection()?.execute("UPDATE cards SET status='CardStale' WHERE asset_id=?1", [asset_id])?;
        self.enqueue_thumbnail(asset_id, 10)
    }

    pub fn regenerate_all_thumbnails(&self) -> CoreResult<serde_json::Value> {
        let motion_ready = self.motion_preview_model()?.is_some();
        let mut cursor = None;
        let (mut queued, mut skipped, mut failed) = (0, 0, 0);
        loop {
            let page = self.list_asset_page(None, None, None, false, cursor.as_ref(), 500, None, None, true)?;
            for asset in page.items {
                if asset.statuses.iter().any(|status| matches!(status.as_str(), "ParseFailed" | "MissingSource" | "Unsupported"))
                    || (asset.asset_type == AssetType::Motion && !motion_ready) {
                    skipped += 1;
                    continue;
                }
                match self.regenerate_thumbnail(&asset.id) {
                    Ok(_) => queued += 1,
                    Err(_) => failed += 1,
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() { break; }
        }
        Ok(serde_json::json!({"queued": queued, "skipped": skipped, "failed": failed}))
    }

    pub fn asset_counts(&self) -> CoreResult<serde_json::Value> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT root_id,asset_type,asset_count FROM asset_counts WHERE asset_count>0"
        )?;
        let mut model = 0_i64;
        let mut motion = 0_i64;
        let mut scene = 0_i64;
        let mut by_root = serde_json::Map::new();
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })? {
            let (root_id, asset_type, count) = row?;
            by_root.insert(root_id, serde_json::json!(count));
            match asset_type.as_str() {
                "model" => model += count,
                "motion" => motion += count,
                "scene" => scene += count,
                _ => {},
            }
        }
        Ok(serde_json::json!({"all":model+motion+scene,"model":model,"motion":motion,"scene":scene,"byRoot":by_root}))
    }

    pub(crate) fn ensure_asset_active(&self, asset_id: &str) -> CoreResult<()> {
        let connection = self.connection()?;
        let retired: Option<bool> = connection
            .query_row(
                "SELECT retired_format FROM assets WHERE id=?1",
                [asset_id],
                |row| row.get(0),
            )
            .optional()?;
        match retired {
            None => Err(CoreError::AssetNotFound(asset_id.to_owned())),
            Some(true) => Err(CoreError::UnsupportedAssetFormat("X".to_owned())),
            Some(false) => Ok(()),
        }
    }

    pub(crate) fn ensure_asset_visible(&self, asset_id: &str) -> CoreResult<()> {
        let connection = self.connection()?;
        let asset: Option<(bool, String)> = connection
            .query_row(
                "SELECT retired_format,visibility FROM assets WHERE id=?1",
                [asset_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match asset {
            None => Err(CoreError::AssetNotFound(asset_id.to_owned())),
            Some((true, _)) => Err(CoreError::UnsupportedAssetFormat("X".to_owned())),
            Some((false, visibility)) if visibility == "normal" => Ok(()),
            Some((false, _)) => Err(CoreError::AssetNotFound(asset_id.to_owned())),
        }
    }

    pub fn asset_directories(&self, root_id: &str) -> CoreResult<Vec<serde_json::Value>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT directory,asset_count FROM asset_directory_counts
             WHERE root_id=?1 AND asset_count>0 ORDER BY directory COLLATE NOCASE"
        )?;
        let rows = statement.query_map([root_id], |row| {
            Ok(serde_json::json!({"path":row.get::<_,String>(0)?, "count":row.get::<_,i64>(1)?}))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn directory_page(
        &self,
        root_id: &str,
        requested_path: Option<&str>,
        recursive_scope: bool,
    ) -> CoreResult<DirectoryPage> {
        let root = self.list_roots()?.into_iter()
            .find(|root| root.id == root_id)
            .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
        let root_path = PathBuf::from(&root.path);
        let mut path = requested_path.map(PathBuf::from).unwrap_or_else(|| root_path.clone());
        if !directory_path_within(&path, &root_path) {
            path = root_path.clone();
        }
        path = normalize_directory_path(&path);
        let mut adjusted = requested_path.is_some_and(|requested| !directory_path_equal(&path, Path::new(requested)));
        while !path.is_dir() && !directory_path_equal(&path, &root_path) {
            adjusted = true;
            if !path.pop() { break; }
        }
        if !path.is_dir() {
            path = root_path.clone();
            adjusted = true;
        }
        if let (Ok(canonical_root), Ok(canonical_path)) = (std::fs::canonicalize(&root_path), std::fs::canonicalize(&path)) {
            if directory_path_within(&canonical_path, &canonical_root) {
                path = canonical_path;
            } else {
                path = root_path.clone();
                adjusted = true;
            }
        }

        let path_text = path.to_string_lossy().into_owned();
        let path_prefix = path_text.trim_end_matches(['\\', '/']).to_owned();
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT directory,asset_count FROM asset_directory_counts
             WHERE root_id=?1 AND (directory=?2 COLLATE NOCASE
               OR substr(directory,1,length(?3)+1)=(?3 || char(92)) COLLATE NOCASE
               OR substr(directory,1,length(?3)+1)=(?3 || '/') COLLATE NOCASE)
             ORDER BY directory COLLATE NOCASE",
        )?;
        let rows = statement.query_map(params![root_id, path_text, path_prefix], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut recursive_count = 0_i64;
        let mut direct_count = 0_i64;
        let mut children = HashMap::<String, i64>::new();
        for row in rows {
            let (directory, count) = row?;
            if directory_path_equal(Path::new(&directory), &path) {
                direct_count += count;
                recursive_count += count;
                continue;
            }
            let Some(suffix) = relative_directory_suffix(&path_text, &directory) else { continue; };
            recursive_count += count;
            let child_name = suffix.split(['\\', '/']).next().unwrap_or_default();
            if child_name.is_empty() { continue; }
            let child_path = path.join(child_name).to_string_lossy().into_owned();
            *children.entry(child_path).or_default() += count;
        }
        let child_directories = children.into_iter()
            .map(|(path, count)| AssetDirectory { path, count })
            .collect::<Vec<_>>();
        Ok(DirectoryPage {
            path: path_text,
            visible_count: if recursive_scope { recursive_count } else { direct_count },
            child_directories,
            adjusted,
        })
    }

    pub fn add_root(
        &self,
        asset_type: AssetType,
        path: &str,
        display_name: Option<&str>,
    ) -> CoreResult<Root> {
        self.add_root_with_recursive(asset_type, path, display_name, true)
    }

    pub fn add_root_with_recursive(
        &self,
        asset_type: AssetType,
        path: &str,
        display_name: Option<&str>,
        scan_recursive: bool,
    ) -> CoreResult<Root> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| CoreError::InvalidRoot(format!("{path}: {error}")))?;
        if !canonical.is_dir() {
            return Err(CoreError::InvalidRoot(format!("{path} is not a directory")));
        }
        let path_text = canonical.to_string_lossy().into_owned();
        let name = display_name
            .map(str::to_owned)
            .or_else(|| {
                canonical
                    .file_name()
                    .map(|value| value.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| path_text.clone());
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        connection.execute(
            "INSERT INTO roots (id, asset_type, path, path_key, display_name, scan_recursive, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, asset_type.as_str(), path_text, path_text.to_lowercase(), name, scan_recursive, now],
        )?;
        Ok(Root {
            id,
            asset_type,
            path: path_text,
            display_name: name,
            enabled: true,
            scan_recursive,
            created_at: now,
            last_scan_at: None,
            scan_status: "NeverScanned".to_owned(),
        })
    }

    pub fn list_roots(&self) -> CoreResult<Vec<Root>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let mut statement = connection.prepare("SELECT id,asset_type,path,display_name,enabled,scan_recursive,created_at,last_scan_at,scan_status FROM roots ORDER BY asset_type,display_name COLLATE NOCASE")?;
        let rows = statement.query_map([], |row| {
            let raw_type: String = row.get(1)?;
            Ok(Root {
                id: row.get(0)?,
                asset_type: AssetType::parse(&raw_type).unwrap_or(AssetType::Model),
                path: row.get(2)?,
                display_name: row.get(3)?,
                enabled: row.get::<_, i64>(4)? != 0,
                scan_recursive: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
                last_scan_at: row.get(7)?,
                scan_status: row.get(8)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn update_root(
        &self,
        root_id: &str,
        enabled: Option<bool>,
        scan_recursive: Option<bool>,
        display_name: Option<&str>,
    ) -> CoreResult<Root> {
        let _scan_guard = self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
        let connection = self.connection()?;
        let current = connection
            .query_row(
                "SELECT id,asset_type,path,display_name,enabled,scan_recursive,created_at,last_scan_at,scan_status FROM roots WHERE id=?1",
                [root_id],
                |row| {
                    let raw_type: String = row.get(1)?;
                    Ok(Root {
                        id: row.get(0)?,
                        asset_type: AssetType::parse(&raw_type).unwrap_or(AssetType::Model),
                        path: row.get(2)?,
                        display_name: row.get(3)?,
                        enabled: row.get::<_, i64>(4)? != 0,
                        scan_recursive: row.get::<_, i64>(5)? != 0,
                        created_at: row.get(6)?,
                        last_scan_at: row.get(7)?,
                        scan_status: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
        let enabled = enabled.unwrap_or(current.enabled);
        let scan_recursive = scan_recursive.unwrap_or(current.scan_recursive);
        let display_name = display_name.unwrap_or(&current.display_name).trim();
        if display_name.is_empty() || display_name.chars().count() > 128 {
            return Err(CoreError::InvalidRoot(
                "root display name must contain 1 to 128 characters".to_owned(),
            ));
        }
        let now_paused = !enabled;
        connection.execute(
            "UPDATE roots SET display_name=?2,enabled=?3,scan_recursive=?4,
               scan_status=CASE WHEN ?3=0 THEN 'Paused'
                 WHEN scan_status='Paused' AND last_scan_at IS NULL THEN 'NeverScanned'
                 WHEN scan_status='Paused' THEN 'Ready' ELSE scan_status END
             WHERE id=?1",
            params![root_id, display_name, enabled, scan_recursive],
        )?;
        Ok(Root {
            id: current.id,
            asset_type: current.asset_type,
            path: current.path,
            display_name: display_name.to_owned(),
            enabled,
            scan_recursive,
            created_at: current.created_at,
            last_scan_at: current.last_scan_at.clone(),
            scan_status: if now_paused {
                "Paused".to_owned()
            } else if current.scan_status == "Paused" && current.last_scan_at.is_none() {
                "NeverScanned".to_owned()
            } else if current.scan_status == "Paused" {
                "Ready".to_owned()
            } else {
                current.scan_status
            },
        })
    }

    pub fn remove_root(&self, root_id: &str) -> CoreResult<bool> {
        let _scan_guard = self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
        let removed = {
            let mut connection = self
                .connection
                .lock()
                .map_err(|_| CoreError::LockPoisoned)?;
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if crate::operations::is_root_operation_active_on(&transaction, root_id)? {
                return Err(CoreError::AssetOperation("此目录有进行中的文件操作，暂时不能移除索引".to_owned()));
            }
            let active: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM scan_state WHERE root_id=?1 AND status IN ('Discovering','Indexing','Verifying','Relations','Pausing','Cancelling'))
                 OR EXISTS(SELECT 1 FROM jobs j JOIN assets a ON a.id=j.asset_id WHERE a.root_id=?1 AND j.status IN ('Pending','Parsing','Rendering','Encoding','Cancelling'))",
                [root_id], |row| row.get(0),
            )?;
            if active { return Err(CoreError::InvalidRoot("此目录仍有后台任务占用，请取消任务并等待退出后再移除索引".to_owned())); }
            let removed = transaction.execute("DELETE FROM roots WHERE id=?1", [root_id])? > 0;
            transaction.commit()?;
            removed
        };
        if removed {
            self.rebuild_relations()?;
        }
        Ok(removed)
    }

    pub fn scan_root(&self, root_id: &str) -> CoreResult<ScanReport> {
        crate::scan_queue::enqueue_inline(self, root_id)?;
        let work = self.claim_pending_scan(root_id)?
            .ok_or_else(|| CoreError::InvalidRoot("该目录已有扫描任务或文件操作占用；请在任务队列中管理".to_owned()))?;
        self.scan_with_work(root_id, &work)
    }

    pub(crate) fn scan_queued_root(
        &self,
        root_id: &str,
        work: &crate::types::ScanWork,
    ) -> CoreResult<ScanReport> {
        self.scan_with_work(root_id, work)
    }

    fn scan_with_work(
        &self,
        root_id: &str,
        work: &crate::types::ScanWork,
    ) -> CoreResult<ScanReport> {
        let _scan_guard = self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
        let root = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| CoreError::LockPoisoned)?;
            connection.query_row(
                "SELECT id,asset_type,path,display_name,enabled,scan_recursive,created_at,last_scan_at,scan_status FROM roots WHERE id=?1",
                [root_id], |row| {
                    let raw_type: String = row.get(1)?;
                    Ok(Root { id: row.get(0)?, asset_type: AssetType::parse(&raw_type).unwrap_or(AssetType::Model),
                        path: row.get(2)?, display_name: row.get(3)?, enabled: row.get::<_, i64>(4)? != 0,
                        scan_recursive: row.get::<_, i64>(5)? != 0, created_at: row.get(6)?, last_scan_at: row.get(7)?, scan_status: row.get(8)? })
                },
            ).optional()?.ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?
        };
        if !root.enabled {
            return Err(CoreError::RootDisabled(root_id.to_owned()));
        }
        let result = scanner::scan(self, &root, work);
        self.finish_scan(root_id, work, &result)?;
        result
    }

    pub fn enqueue_scan(&self, root_id: &str) -> CoreResult<ScanState> {
        crate::scan_queue::enqueue(self, root_id, false)
    }

    pub fn enqueue_full_check(&self, root_id: &str) -> CoreResult<ScanState> {
        crate::scan_queue::enqueue(self, root_id, true)
    }

    pub fn enqueue_scan_changes(
        &self,
        root_id: &str,
        changes: &[crate::types::ScanChange],
    ) -> CoreResult<Option<ScanState>> {
        crate::scan_queue::enqueue_changes(self, root_id, changes)
    }

    pub fn resume_scan_jobs(&self) -> CoreResult<()> {
        crate::scan_queue::resume(self)
    }

    pub fn cancel_scan(&self, root_id: &str) -> CoreResult<bool> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE scan_state SET status=CASE WHEN status IN ('Pending','Paused') THEN 'Cancelled' ELSE 'Cancelling' END,
               error_json=NULL,updated_at=?2
             WHERE root_id=?1 AND status IN ('Pending','Paused','Pausing','Discovering','Indexing','Verifying','Relations')",
            params![root_id, now],
        )?;
        if changed > 0 {
            connection.execute("UPDATE roots SET scan_status='Cancelled' WHERE id=?1", [root_id])?;
        }
        Ok(changed > 0)
    }

    pub fn pause_scan(&self, root_id: &str) -> CoreResult<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE scan_state SET status=CASE WHEN status='Pending' THEN 'Paused' ELSE 'Pausing' END,
               updated_at=?2 WHERE root_id=?1 AND status IN
               ('Pending','Discovering','Indexing','Verifying','Relations')",
            params![root_id, Utc::now().to_rfc3339()],
        )?;
        if changed > 0 {
            connection.execute("UPDATE roots SET scan_status='Paused' WHERE id=?1", [root_id])?;
        }
        Ok(changed > 0)
    }

    pub fn continue_scan(&self, root_id: &str) -> CoreResult<ScanState> {
        crate::scan_queue::continue_paused(self, root_id)
    }

    pub fn move_pending_scan(&self, root_id: &str, direction: i32) -> CoreResult<bool> {
        crate::scan_queue::move_pending(self, root_id, direction)
    }

    pub fn list_scan_states(&self) -> CoreResult<Vec<ScanState>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.root_id,s.status,s.progress,s.files_seen,s.files_processed,s.error_json,s.updated_at,s.queue_order,s.full_check,
               CASE WHEN EXISTS(SELECT 1 FROM scan_changes c WHERE c.root_id=s.root_id AND c.scope='root') THEN 'full'
                    WHEN EXISTS(SELECT 1 FROM scan_changes c WHERE c.root_id=s.root_id) THEN 'local' ELSE 'none' END
             FROM scan_state s ORDER BY CASE WHEN s.status='Pending' THEN 0 ELSE 1 END,
             CASE WHEN s.status='Pending' THEN s.queue_order END,s.updated_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let error_json: Option<String> = row.get(5)?;
            Ok(ScanState {
                root_id: row.get(0)?,
                status: row.get(1)?,
                queue_order: row.get(7)?,
                full_check: row.get::<_, i64>(8)? != 0,
                scope: row.get(9)?,
                progress: row.get(2)?,
                files_seen: row.get::<_, i64>(3)?.max(0) as usize,
                files_processed: row.get::<_, i64>(4)?.max(0) as usize,
                error: error_json.and_then(|value| {
                    serde_json::from_str::<serde_json::Value>(&value).ok()?
                        .get("message")?.as_str().map(str::to_owned)
                }),
                updated_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn claim_pending_scan(
        &self,
        root_id: &str,
    ) -> CoreResult<Option<crate::types::ScanWork>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if crate::operations::is_root_operation_active_on(&transaction, root_id)? { return Ok(None); }
        let changed = transaction.execute(
            "UPDATE scan_state SET status='Discovering',claim_owner=?3,updated_at=?2
             WHERE root_id=?1 AND status='Pending'",
            params![root_id, Utc::now().to_rfc3339(), i64::from(std::process::id())],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute("UPDATE roots SET scan_status='Scanning' WHERE id=?1", [root_id])?;
        let (generation, full_check): (i64, bool) = transaction.query_row(
            "SELECT dirty_generation,full_check!=0 FROM scan_state WHERE root_id=?1",
            [root_id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let changes = {
            let mut statement = transaction.prepare(
                "SELECT path,scope FROM scan_changes WHERE root_id=?1 AND generation<=?2 ORDER BY scope,path_key",
            )?;
            statement.query_map(params![root_id, generation], |row| {
                Ok(crate::types::PendingScanChange { path: row.get(0)?, scope: row.get(1)? })
            })?.collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(Some(crate::types::ScanWork { generation, full_check, changes }))
    }

    pub(crate) fn update_scan_progress(
        &self, root_id: &str, status: &str, progress: f64, files_seen: usize, files_processed: usize,
    ) -> CoreResult<()> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE scan_state SET status=?2,progress=?3,files_seen=?4,files_processed=?5,updated_at=?6
             WHERE root_id=?1 AND status NOT IN ('Cancelled','Cancelling','Paused','Pausing')",
            params![root_id, status, progress.clamp(0.0, 1.0), files_seen as i64,
                files_processed as i64, Utc::now().to_rfc3339()],
        )?;
        if changed == 0 { return Err(self.scan_interruption_locked(&connection, root_id)?); }
        Ok(())
    }

    fn scan_interruption_locked(&self, connection: &Connection, root_id: &str) -> CoreResult<CoreError> {
        let status: Option<String> = connection.query_row(
            "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
        ).optional()?;
        Ok(if matches!(status.as_deref(), Some("Paused" | "Pausing")) {
            CoreError::ScanPaused
        } else {
            CoreError::ScanCancelled
        })
    }

    pub(crate) fn finish_scan(
        &self,
        root_id: &str,
        work: &crate::types::ScanWork,
        result: &CoreResult<ScanReport>,
    ) -> CoreResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        match result {
            Ok(report) => {
                let status: Option<String> = transaction.query_row(
                    "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
                ).optional()?;
                if matches!(status.as_deref(), Some("Paused" | "Pausing" | "Cancelled" | "Cancelling")) {
                    let paused = matches!(status.as_deref(), Some("Paused" | "Pausing"));
                    let status = if paused { "Paused" } else { "Cancelled" };
                    transaction.execute("UPDATE scan_state SET status=?2,claim_owner=NULL,updated_at=?3 WHERE root_id=?1",
                        params![root_id, status, now])?;
                    transaction.execute("UPDATE roots SET scan_status=?2 WHERE id=?1", params![root_id, status])?;
                } else {
                    transaction.execute(
                        "DELETE FROM scan_changes WHERE root_id=?1 AND generation<=?2",
                        params![root_id, work.generation],
                    )?;
                    let has_pending: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM scan_changes WHERE root_id=?1)", [root_id], |row| row.get(0),
                    )?;
                    if has_pending {
                        let queue_order: i64 = transaction.query_row(
                            "SELECT COALESCE(MAX(queue_order),0)+1 FROM scan_state", [], |row| row.get(0),
                        )?;
                        transaction.execute(
                            "UPDATE scan_state SET status='Pending',progress=0,files_seen=0,files_processed=0,
                             full_check=0,error_json=NULL,claim_owner=NULL,queue_order=?2,updated_at=?3 WHERE root_id=?1",
                            params![root_id, queue_order, now],
                        )?;
                        transaction.execute("UPDATE roots SET scan_status='Pending' WHERE id=?1", [root_id])?;
                    } else {
                        transaction.execute("UPDATE scan_state SET status='Completed',progress=1,files_seen=?2,
                            files_processed=?2,full_check=0,error_json=NULL,claim_owner=NULL,updated_at=?3 WHERE root_id=?1",
                            params![root_id, report.files_seen as i64, now])?;
                        transaction.execute("UPDATE roots SET scan_status='Ready',last_scan_at=?2 WHERE id=?1",
                            params![root_id, now])?;
                    }
                }
            }
            Err(CoreError::ScanCancelled) => {
                transaction.execute("UPDATE scan_state SET status='Cancelled',claim_owner=NULL,updated_at=?2 WHERE root_id=?1",
                    params![root_id, now])?;
                transaction.execute("UPDATE roots SET scan_status='Cancelled' WHERE id=?1", [root_id])?;
            }
            Err(CoreError::ScanPaused) => {
                let status: Option<String> = transaction.query_row(
                    "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
                ).optional()?;
                let final_status = match status.as_deref() {
                    Some("Pausing" | "Paused") => Some("Paused"),
                    Some("Cancelling" | "Cancelled") => Some("Cancelled"),
                    _ => None,
                };
                if let Some(final_status) = final_status {
                    transaction.execute("UPDATE scan_state SET status=?2,claim_owner=NULL,updated_at=?3 WHERE root_id=?1",
                        params![root_id, final_status, now])?;
                    transaction.execute("UPDATE roots SET scan_status=?2 WHERE id=?1",
                        params![root_id, final_status])?;
                }
            }
            Err(error) => {
                transaction.execute("UPDATE scan_state SET status='Failed',error_json=?2,claim_owner=NULL,updated_at=?3 WHERE root_id=?1",
                    params![root_id, serde_json::json!({"message": error.to_string()}).to_string(), now])?;
                transaction.execute("UPDATE roots SET scan_status='Failed' WHERE id=?1", [root_id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn model_preview(&self, asset_id: &str) -> CoreResult<Vec<u8>> {
        let asset = self.inspect_asset(asset_id)?;
        if asset.asset_type != AssetType::Model
            || !crate::model_io::is_model_path(Path::new(&asset.primary_source))
        {
            return Err(CoreError::ModelPreview(
                "3D 模型预览支持 PMX 和 PMD 资产".to_owned(),
            ));
        }
        self.model_preview_file(Path::new(&asset.primary_source))
    }

    pub fn motion_preview_frame(&self, asset_id: &str, frame: u32) -> CoreResult<serde_json::Value> {
        crate::motion_view::frame(self, asset_id, frame)
    }

    pub fn scene_preview(&self, asset_id: &str) -> CoreResult<Vec<u8>> {
        let asset = self.inspect_asset(asset_id)?;
        if asset.asset_type != AssetType::Scene {
            return Err(CoreError::ModelPreview("3D 场景预览需要场景资产".to_owned()));
        }
        crate::thumbnail::scene_preview_file(Path::new(&asset.primary_source))
    }

    pub fn scene_preview_texture(&self, asset_id: &str, texture_path: &str) -> CoreResult<Option<(Vec<u8>, u8)>> {
        let asset = self.inspect_asset(asset_id)?;
        if asset.asset_type != AssetType::Scene || texture_path.len() > 4096 {
            return Err(CoreError::ModelPreview("无效的场景或贴图路径".to_owned()));
        }
        crate::thumbnail::scene_preview_texture_file(Path::new(&asset.primary_source), texture_path)
    }

    pub fn model_preview_file(&self, path: &Path) -> CoreResult<Vec<u8>> {
        if path.extension().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case("pmd")) {
            return crate::thumbnail::pmd_preview_file(path);
        }
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
        {
            return Err(CoreError::ModelPreview(
                "3D preview currently supports PMX model files".to_owned(),
            ));
        }
        let metadata = std::fs::metadata(path)?;
        if metadata.len() > 512 * 1024 * 1024 {
            return Err(CoreError::ModelPreview(
                "PMX file exceeds the 512 MiB preview limit".to_owned(),
            ));
        }
        let bytes = read_source(path)?;
        let parsed =
            parse_pmx_model(&bytes).map_err(|error| CoreError::ModelPreview(error.to_string()))?;
        crate::thumbnail::remember_preview_textures(path, &metadata, parsed.materials.iter().flat_map(|material| [material.texture_path.clone(), material.sphere_texture_path.clone(), material.toon_texture_path.clone()]));
        let geometry = parsed.geometry;
        let vertex_count = geometry.positions.len() / 3;
        if geometry.positions.len() != vertex_count * 3
            || geometry.normals.len() != vertex_count * 3
            || geometry.uvs.len() != vertex_count * 2
            || geometry.skin_indices.len() != vertex_count * 4
            || geometry.skin_weights.len() != vertex_count * 4
            || geometry.sdef.skinning_modes.len() != vertex_count
            || geometry.sdef.c.len() != vertex_count * 3
            || geometry.sdef.r0.len() != vertex_count * 3
            || geometry.sdef.r1.len() != vertex_count * 3
            || geometry.indices.len() % 3 != 0
            || geometry.qdef.enabled.len() != vertex_count
        {
            return Err(CoreError::ModelPreview(
                "PMX mesh has inconsistent geometry or weight data".to_owned(),
            ));
        }
        if geometry.indices.iter().any(|index| *index as usize >= vertex_count) {
            return Err(CoreError::ModelPreview("PMX 三角形引用了不存在的顶点".to_owned()));
        }
        let group_count = geometry.material_groups.len();
        let texture_paths = geometry.material_groups.iter().map(|group| {
            parsed.materials.get(group.material_index)
                .map(|material| material.texture_path.as_str()).unwrap_or("")
        }).collect::<Vec<_>>();
        let bone_names = parsed
            .skeleton
            .bones
            .iter()
            .map(|bone| {
                if bone.name.trim().is_empty() {
                    bone.english_name.as_str()
                } else {
                    bone.name.as_str()
                }
            })
            .collect::<Vec<_>>();
        let bone_payload_len = bone_names.iter().try_fold(0usize, |size, name| {
            size.checked_add(20)?.checked_add(name.len())
        });
        let texture_payload_len = texture_paths.iter().try_fold(0usize, |size, path| {
            size.checked_add(4)?.checked_add(path.len())
        });
        let payload_len = 24usize
            .checked_add(vertex_count.checked_mul(104).unwrap_or(usize::MAX))
            .and_then(|size| size.checked_add(geometry.indices.len().checked_mul(4)?))
            .and_then(|size| size.checked_add(group_count.checked_mul(24)?))
            .and_then(|size| size.checked_add(bone_payload_len?))
            .and_then(|size| size.checked_add(texture_payload_len?))
            .ok_or_else(|| CoreError::ModelPreview("PMX preview size overflow".to_owned()))?;
        if payload_len > 256 * 1024 * 1024 {
            return Err(CoreError::ModelPreview(
                "PMX mesh exceeds the 256 MiB preview limit".to_owned(),
            ));
        }

        let mut output = Vec::with_capacity(payload_len);
        output.extend_from_slice(b"MMDV");
        output.extend_from_slice(&3u32.to_le_bytes());
        output.extend_from_slice(&(vertex_count as u32).to_le_bytes());
        output.extend_from_slice(&(geometry.indices.len() as u32).to_le_bytes());
        output.extend_from_slice(&(group_count as u32).to_le_bytes());
        output.extend_from_slice(&(bone_names.len() as u32).to_le_bytes());
        for vertex in 0..vertex_count {
            for value in geometry.positions[vertex * 3..vertex * 3 + 3]
                .iter()
                .chain(&geometry.normals[vertex * 3..vertex * 3 + 3])
                .chain(&geometry.uvs[vertex * 2..vertex * 2 + 2])
            {
                let value = if value.is_finite() { *value } else { 0.0 };
                output.extend_from_slice(&value.to_le_bytes());
            }
            for index in &geometry.skin_indices[vertex * 4..vertex * 4 + 4] {
                if *index > 16_777_216 {
                    return Err(CoreError::ModelPreview(
                        "bone index exceeds the exact preview range".to_owned(),
                    ));
                }
                output.extend_from_slice(&(*index as f32).to_le_bytes());
            }
            for value in &geometry.skin_weights[vertex * 4..vertex * 4 + 4] {
                let value = if value.is_finite() { *value } else { 0.0 };
                output.extend_from_slice(&value.to_le_bytes());
            }
            let mode = match geometry.sdef.skinning_modes[vertex].as_str() {
                "bdef1" => 0u32,
                "bdef2" => 1,
                "bdef4" => 2,
                "sdef" => 3,
                "qdef" => 4,
                other => {
                    return Err(CoreError::ModelPreview(format!(
                        "unsupported PMX skinning mode: {other}"
                    )));
                }
            };
            output.extend_from_slice(&(mode as f32).to_le_bytes());
            for value in geometry.sdef.c[vertex * 3..vertex * 3 + 3]
                .iter()
                .chain(&geometry.sdef.r0[vertex * 3..vertex * 3 + 3])
                .chain(&geometry.sdef.r1[vertex * 3..vertex * 3 + 3])
            {
                let value = if value.is_finite() { *value } else { 0.0 };
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        for index in &geometry.indices {
            output.extend_from_slice(&index.to_le_bytes());
        }
        for group in &geometry.material_groups {
            let start = u32::try_from(group.start).map_err(|_| {
                CoreError::ModelPreview("material group offset exceeds 32-bit range".to_owned())
            })?;
            let count = u32::try_from(group.count).map_err(|_| {
                CoreError::ModelPreview("material group size exceeds 32-bit range".to_owned())
            })?;
            if group
                .start
                .checked_add(group.count)
                .is_none_or(|end| end > geometry.indices.len())
            {
                return Err(CoreError::ModelPreview(
                    "material group exceeds the PMX index buffer".to_owned(),
                ));
            }
            output.extend_from_slice(&start.to_le_bytes());
            output.extend_from_slice(&count.to_le_bytes());
            let color = parsed
                .materials
                .get(group.material_index)
                .map(|material| material.diffuse)
                .unwrap_or([0.72, 0.76, 0.79, 1.0]);
            for value in color {
                let value = if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        for (bone, name) in parsed.skeleton.bones.iter().zip(bone_names) {
            let name_length = u32::try_from(name.len()).map_err(|_| {
                CoreError::ModelPreview("bone name exceeds 32-bit range".to_owned())
            })?;
            output.extend_from_slice(&bone.parent_index.to_le_bytes());
            for value in bone.position {
                let value = if value.is_finite() { value } else { 0.0 };
                output.extend_from_slice(&value.to_le_bytes());
            }
            output.extend_from_slice(&name_length.to_le_bytes());
            output.extend_from_slice(name.as_bytes());
        }
        for path in texture_paths {
            let length = u32::try_from(path.len()).map_err(|_| {
                CoreError::ModelPreview("texture path exceeds 32-bit range".to_owned())
            })?;
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(path.as_bytes());
        }
        Ok(output)
    }

    pub fn model_preview_texture_file(&self, model_path: &Path, texture_path: &str) -> CoreResult<Option<(Vec<u8>, u8)>> {
        if !crate::model_io::is_model_path(model_path)
            || texture_path.len() > 4096 {
            return Err(CoreError::ModelPreview("invalid model or texture path".to_owned()));
        }
        crate::thumbnail::check_preview_texture(model_path, texture_path)?;
        crate::thumbnail::preview_texture_png(model_path, texture_path)
    }

    pub fn motion_preview_model(&self) -> CoreResult<Option<String>> {
        let connection = self.connection()?;
        let value_json: Option<String> = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key='motion_preview_model'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value_json
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn thumbnail_concurrency(&self) -> CoreResult<ThumbnailConcurrencySettings> {
        let connection = self.connection()?;
        let value_json: Option<String> = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key='thumbnail_concurrency'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let settings: ThumbnailConcurrencySettings = value_json
            .map(|value| serde_json::from_str(&value))
            .transpose()?
            .unwrap_or_default();
        settings.validate()?;
        crate::thumbnail_concurrency::configure(settings.clone());
        Ok(settings)
    }

    pub fn set_thumbnail_concurrency(
        &self,
        settings: &ThumbnailConcurrencySettings,
    ) -> CoreResult<ThumbnailConcurrencySettings> {
        settings.validate()?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO settings(key,value_json,updated_at) VALUES ('thumbnail_concurrency',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
            params![serde_json::to_string(settings)?, Utc::now().to_rfc3339()],
        )?;
        crate::thumbnail_concurrency::configure(settings.clone());
        Ok(settings.clone())
    }

    pub fn set_motion_preview_model(&self, path: Option<&str>) -> CoreResult<Option<String>> {
        let normalized_path = if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
            let source = Path::new(path);
            if !crate::model_io::is_model_path(source)
            {
                return Err(CoreError::ThumbnailRender(
                    "动作预览模型必须是 PMX 或 PMD 文件".to_owned(),
                ));
            }
            let metadata = std::fs::metadata(source)?;
            if metadata.len() > 512 * 1024 * 1024 {
                return Err(CoreError::ThumbnailRender(
                    "动作预览模型超过 512 MiB 解析上限".to_owned(),
                ));
            }
            let bytes = read_source(source)?;
            if source.extension().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case("pmd")) {
                crate::model_io::parse_pmd_model(&bytes).map_err(CoreError::ThumbnailRender)?;
            } else {
                parse_pmx_model(&bytes).map_err(CoreError::ThumbnailRender)?;
            }
            crate::model_io::import_model_runtime(&bytes).map_err(|error| {
                CoreError::ThumbnailRender(format!("动作预览模型骨架不可用：{error}"))
            })?;
            Some(
                std::fs::canonicalize(source)?
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        };

        let connection = self.connection()?;
        if let Some(path) = normalized_path.as_deref() {
            connection.execute(
                "INSERT INTO settings(key,value_json,updated_at) VALUES ('motion_preview_model',?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
                params![serde_json::to_string(path)?, Utc::now().to_rfc3339()],
            )?;
        } else {
            connection.execute("DELETE FROM settings WHERE key='motion_preview_model'", [])?;
        }
        connection.execute(
            "UPDATE cards SET status='CardStale',last_checked_at=?1
             WHERE status='CardValid' AND asset_id IN (
               SELECT id FROM assets WHERE asset_type='motion'
                 AND (lower(primary_source) LIKE '%.vpd' OR lower(primary_source) LIKE '%.vmd')
             )",
            [Utc::now().to_rfc3339()],
        )?;
        Ok(normalized_path)
    }

    pub fn list_assets(
        &self,
        asset_type: Option<AssetType>,
        query: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<Asset>> {
        scanner::list_assets(self, asset_type, query, limit)
    }

    pub fn list_asset_page(
        &self,
        asset_type: Option<AssetType>,
        query: Option<&str>,
        root_id: Option<&str>,
        favorite_only: bool,
        cursor: Option<&AssetCursor>,
        limit: usize,
        motion_format: Option<&str>,
        directory_path: Option<&str>,
        recursive_scope: bool,
    ) -> CoreResult<AssetPage> {
        scanner::list_asset_page(
            self,
            asset_type,
            query,
            root_id,
            favorite_only,
            cursor,
            limit,
            motion_format,
            directory_path,
            recursive_scope,
        )
    }

    pub fn inspect_asset(&self, asset_id: &str) -> CoreResult<Asset> {
        scanner::inspect_asset(self, asset_id)
    }

    pub fn pending_cards(&self) -> CoreResult<Vec<Asset>> {
        Ok(self
            .list_assets(None, None, scanner::MAX_LIST_ITEMS)?
            .into_iter()
            .filter(|asset| {
                matches!(
                    asset.card_status.as_str(),
                    "CardMissing" | "CardStale" | "CardBroken"
                ) || !asset.has_thumbnail
            })
            .collect())
    }

    pub fn create_card(
        &self,
        asset_id: &str,
        preview_webp: Option<&[u8]>,
    ) -> CoreResult<CardResult> {
        crate::cards::create(self, asset_id, preview_webp, None)
    }

    pub fn create_card_with_thumbnail(&self, asset_id: &str) -> CoreResult<CardResult> {
        let mut no_progress = |_: &str, _: f64| true;
        self.create_card_with_thumbnail_progress(asset_id, &mut no_progress)
    }

    pub(crate) fn create_card_with_thumbnail_progress(
        &self,
        asset_id: &str,
        progress: &mut dyn FnMut(&str, f64) -> bool,
    ) -> CoreResult<CardResult> {
        if crate::operations::is_asset_operation_active(self, asset_id)? {
            return Err(CoreError::AssetOperation(
                "该资产正在执行文件操作，暂时不能创建资源卡".to_owned(),
            ));
        }
        self.ensure_asset_active(asset_id)?;
        let asset = self.inspect_asset(asset_id)?;
        let extension = Path::new(&asset.primary_source)
            .extension()
            .and_then(|extension| extension.to_str());
        let is_supported_thumbnail = extension
            .is_some_and(|extension| asset.asset_type.supports_thumbnail_extension(extension));
        if is_supported_thumbnail {
            let motion_preview_model = if asset.asset_type == AssetType::Motion {
                Some(self.motion_preview_model()?.ok_or_else(|| {
                    CoreError::ThumbnailRender(
                        "请先在设置中指定 动作预览模型（PMX / PMD）".to_owned(),
                    )
                })?)
            } else {
                None
            };
            let preview_settings_version = if let Some(model_path) = motion_preview_model.as_deref()
            {
                crate::thumbnail::motion_preview_settings_version(Path::new(model_path))?
            } else if asset.asset_type == AssetType::Scene {
                crate::thumbnail::SCENE_PREVIEW_SETTINGS_VERSION.to_owned()
            } else {
                crate::thumbnail::PREVIEW_SETTINGS_VERSION.to_owned()
            };
            if let Some((preview, report)) =
                crate::cards::cached_thumbnail(self, asset_id, &preview_settings_version)?
            {
                if !progress("Encoding", 0.95) {
                    return Err(CoreError::ThumbnailCancelled);
                }
                return crate::cards::create_for_asset(self, &asset, Some(&preview), Some(&report));
            }
            let generated = if let Some(model_path) = motion_preview_model.as_deref() {
                match extension.map(str::to_ascii_lowercase).as_deref() {
                    Some("vpd") => crate::thumbnail::render_vpd_motion_file_with_progress(
                        Path::new(&asset.primary_source),
                        Path::new(model_path),
                        progress,
                    )?,
                    Some("vmd") => crate::thumbnail::render_vmd_motion_file_with_progress(
                        Path::new(&asset.primary_source),
                        Path::new(model_path),
                        progress,
                    )?,
                    _ => unreachable!("unsupported thumbnail extension passed the type gate"),
                }
            } else {
                crate::thumbnail::render_file_with_progress(
                    Path::new(&asset.primary_source),
                    asset.asset_type == AssetType::Scene,
                    progress,
                )?
            };
            if !progress("Encoding", 0.99) {
                return Err(CoreError::ThumbnailCancelled);
            }
            crate::cards::create_for_asset(
                self,
                &asset,
                Some(&generated.preview_webp),
                Some(&generated.report),
            )
        } else {
            crate::cards::create_for_asset(self, &asset, None, None)
        }
    }

    pub fn card_thumbnail(&self, asset_id: &str) -> CoreResult<Option<Vec<u8>>> {
        crate::cards::read_thumbnail(self, asset_id)
    }

    pub fn enqueue_thumbnail(
        &self,
        asset_id: &str,
        priority: i32,
    ) -> CoreResult<serde_json::Value> {
        crate::jobs::enqueue_thumbnail(self, asset_id, priority)
    }

    pub fn resume_thumbnail_jobs(&self) -> CoreResult<()> {
        self.thumbnail_concurrency()?;
        crate::operations::mark_interrupted(self)?;
        crate::jobs::start_worker(self.clone())
    }

    pub fn enqueue_thumbnails(
        &self,
        asset_ids: &[String],
        priority: i32,
    ) -> CoreResult<Vec<serde_json::Value>> {
        crate::jobs::enqueue_thumbnails(self, asset_ids, priority)
    }

    pub fn queue_pending_cards(&self, root_id: &str) -> CoreResult<Vec<String>> {
        let root = self.list_roots()?.into_iter()
            .find(|root| root.id == root_id)
            .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
        if root.asset_type == AssetType::Motion && self.motion_preview_model()?.is_none() {
            return Err(CoreError::ThumbnailRender(
                "请先在设置中指定 动作预览模型（PMX / PMD）".to_owned(),
            ));
        }
        let renderer_revision = crate::cards::expected_renderer_revision(self, root.asset_type)?;
        let mut cursor = None;
        let mut job_ids = Vec::new();
        loop {
            let page = self.list_asset_page(Some(root.asset_type), None, Some(root_id), false, cursor.as_ref(), 500, None, None, true)?;
            for asset in page.items {
                if asset.statuses.iter().any(|status| matches!(status.as_str(),
                    "MissingSource" | "ParseFailed" | "Unsupported")) { continue; }
                let extension = Path::new(&asset.primary_source).extension().and_then(|value| value.to_str());
                if !extension.is_some_and(|value| asset.asset_type.supports_thumbnail_extension(value)) { continue; }
                let validation = match crate::cards::verify_if_changed(
                    self,
                    &asset.id,
                    &renderer_revision,
                )? {
                    Some(validation) => validation,
                    None => crate::cards::verify_with_renderer_revision(
                        self,
                        &asset.id,
                        &renderer_revision,
                    )?,
                };
                if validation.status == "CardValid" && validation.has_thumbnail { continue; }
                let job = self.enqueue_thumbnail(&asset.id, 0)?;
                if let Some(id) = job.get("id").and_then(serde_json::Value::as_str) {
                    job_ids.push(id.to_owned());
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() { break; }
        }
        Ok(job_ids)
    }

    pub fn cancel_job(&self, job_id: &str) -> CoreResult<bool> {
        crate::jobs::cancel_job(self, job_id)
    }

    pub fn retry_job(&self, job_id: &str) -> CoreResult<serde_json::Value> {
        crate::jobs::retry_job(self, job_id)
    }

    pub fn wait_for_job(&self, job_id: &str, timeout: Duration) -> CoreResult<serde_json::Value> {
        crate::jobs::wait_for_job(self, job_id, timeout)
    }

    pub fn render_thumbnail(&self, asset_id: &str) -> CoreResult<GeneratedThumbnail> {
        let asset = self.inspect_asset(asset_id)?;
        let extension = Path::new(&asset.primary_source)
            .extension()
            .and_then(|extension| extension.to_str());
        let supported = extension
            .is_some_and(|extension| asset.asset_type.supports_thumbnail_extension(extension));
        if !supported {
            return Err(CoreError::ThumbnailRender(
                "当前渲染器支持 PMX / PMD 模型和场景，以及配置了 Motion Preview Model 的 VMD/VPD 动作".to_owned(),
            ));
        }
        if asset.asset_type == AssetType::Motion {
            let model_path = self.motion_preview_model()?.ok_or_else(|| {
                CoreError::ThumbnailRender(
                    "请先在设置中指定 动作预览模型（PMX / PMD）".to_owned(),
                )
            })?;
            match extension.map(str::to_ascii_lowercase).as_deref() {
                Some("vpd") => crate::thumbnail::render_vpd_motion_file_with_progress(
                    Path::new(&asset.primary_source),
                    Path::new(&model_path),
                    &mut |_, _| true,
                ),
                Some("vmd") => crate::thumbnail::render_vmd_motion_file_with_progress(
                    Path::new(&asset.primary_source),
                    Path::new(&model_path),
                    &mut |_, _| true,
                ),
                _ => unreachable!("unsupported thumbnail extension passed the type gate"),
            }
        } else {
            if asset.asset_type == AssetType::Scene {
                crate::thumbnail::render_file_with_progress(
                    Path::new(&asset.primary_source), true, &mut |_, _| true,
                )
            } else {
                crate::thumbnail::render_file(Path::new(&asset.primary_source))
            }
        }
    }

    pub fn render_thumbnail_file(&self, path: impl AsRef<Path>) -> CoreResult<GeneratedThumbnail> {
        crate::thumbnail::render_file(path.as_ref())
    }

    pub fn verify_card(&self, asset_id: &str) -> CoreResult<CardValidation> {
        crate::cards::verify(self, asset_id)
    }

    pub fn list_asset_tags(&self, asset_id: &str) -> CoreResult<Vec<AssetTag>> {
        let connection = self.connection()?;
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
            [asset_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(CoreError::AssetNotFound(asset_id.to_owned()));
        }
        let mut statement = connection.prepare(
            "SELECT t.name,at.source,at.confidence FROM asset_tags at
             JOIN tags t ON t.id=at.tag_id WHERE at.asset_id=?1 ORDER BY t.name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([asset_id], |row| {
            Ok(AssetTag {
                name: row.get(0)?,
                source: row.get(1)?,
                confidence: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_asset_tag_overrides(&self, asset_id: &str) -> CoreResult<Vec<String>> {
        self.ensure_asset_visible(asset_id)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT normalized_name FROM asset_tag_overrides WHERE asset_id=?1 ORDER BY normalized_name",
        )?;
        statement.query_map([asset_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn ensure_active_asset_in_transaction(
        transaction: &Transaction<'_>,
        asset_id: &str,
    ) -> CoreResult<()> {
        let asset: Option<(bool, String)> = transaction
            .query_row(
                "SELECT retired_format,visibility FROM assets WHERE id=?1",
                [asset_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match asset {
            None => Err(CoreError::AssetNotFound(asset_id.to_owned())),
            Some((true, _)) => Err(CoreError::UnsupportedAssetFormat("X".to_owned())),
            Some((false, visibility)) if visibility == "normal" => Ok(()),
            Some((false, _)) => Err(CoreError::AssetNotFound(asset_id.to_owned())),
        }
    }

    pub fn add_asset_tag(
        &self,
        asset_id: &str,
        name: &str,
        source: &str,
        confidence: Option<f64>,
    ) -> CoreResult<TagMutation> {
        let name = validate_tag(name)?;
        validate_tag_request(source, confidence)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let mutation =
            Self::add_asset_tag_in_transaction(&transaction, asset_id, &name, source, confidence)?;
        transaction.commit()?;
        Ok(mutation)
    }

    pub fn add_asset_tag_batch(
        &self,
        asset_ids: &[String],
        name: &str,
        source: &str,
        confidence: Option<f64>,
    ) -> CoreResult<Vec<TagMutation>> {
        let name = validate_tag(name)?;
        validate_tag_request(source, confidence)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let mut seen = HashSet::with_capacity(asset_ids.len());
        let mut mutations = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            if seen.insert(asset_id.as_str()) {
                mutations.push(Self::add_asset_tag_in_transaction(
                    &transaction,
                    asset_id,
                    &name,
                    source,
                    confidence,
                )?);
            }
        }
        transaction.commit()?;
        Ok(mutations)
    }

    pub(crate) fn add_asset_tag_in_transaction(
        transaction: &Transaction<'_>,
        asset_id: &str,
        name: &str,
        source: &str,
        confidence: Option<f64>,
    ) -> CoreResult<TagMutation> {
        let normalized_name = name.to_lowercase();
        Self::ensure_active_asset_in_transaction(transaction, asset_id)?;
        let blocked = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM asset_tag_overrides WHERE asset_id=?1 AND normalized_name=?2)",
            params![asset_id, normalized_name],
            |row| row.get::<_, bool>(0),
        )?;
        if blocked && source != "user" {
            return Ok(TagMutation {
                asset_id: asset_id.to_owned(),
                name: name.to_owned(),
                source: source.to_owned(),
                changed: false,
                blocked_by_user: true,
            });
        }
        let existing = transaction
            .query_row(
                "SELECT t.id,at.source FROM tags t LEFT JOIN asset_tags at
                 ON at.tag_id=t.id AND at.asset_id=?1 WHERE t.name=?2 COLLATE NOCASE LIMIT 1",
                params![asset_id, name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        if let Some((_, Some(existing_source))) = &existing
            && existing_source == "user"
            && source != "user"
        {
            return Ok(TagMutation {
                asset_id: asset_id.to_owned(),
                name: name.to_owned(),
                source: existing_source.clone(),
                changed: false,
                blocked_by_user: true,
            });
        }
        let now = Utc::now().to_rfc3339();
        let tag_id = if let Some((id, _)) = existing {
            id
        } else {
            let id = Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO tags(id,name,source,confidence,created_at) VALUES (?1,?2,?3,?4,?5)",
                params![id, name, source, confidence, now],
            )?;
            id
        };
        let changed = transaction.execute(
            "INSERT INTO asset_tags(asset_id,tag_id,source,confidence) VALUES (?1,?2,?3,?4)
             ON CONFLICT(asset_id,tag_id) DO UPDATE SET source=excluded.source,confidence=excluded.confidence
             WHERE asset_tags.source<>excluded.source OR asset_tags.confidence IS NOT excluded.confidence",
                params![asset_id, tag_id, source, confidence],
        )? > 0;
        if changed {
            transaction.execute(
                "UPDATE cards SET status='CardStale',last_checked_at=?2 WHERE asset_id=?1 AND status='CardValid'",
                params![asset_id, now],
            )?;
        }
        if source == "user" {
            transaction.execute(
                "DELETE FROM asset_tag_overrides WHERE asset_id=?1 AND normalized_name=?2",
                params![asset_id, normalized_name],
            )?;
            transaction.execute(
                "UPDATE tags SET source='user',confidence=NULL WHERE id=?1",
                [&tag_id],
            )?;
        }
        Ok(TagMutation {
            asset_id: asset_id.to_owned(),
            name: name.to_owned(),
            source: source.to_owned(),
            changed,
            blocked_by_user: false,
        })
    }

    pub fn remove_asset_tag(&self, asset_id: &str, name: &str) -> CoreResult<TagMutation> {
        let name = validate_tag(name)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let mutation = Self::remove_asset_tag_in_transaction(&transaction, asset_id, &name)?;
        transaction.commit()?;
        Ok(mutation)
    }

    pub fn remove_asset_tag_batch(
        &self,
        asset_ids: &[String],
        name: &str,
    ) -> CoreResult<Vec<TagMutation>> {
        let name = validate_tag(name)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let mut seen = HashSet::with_capacity(asset_ids.len());
        let mut mutations = Vec::with_capacity(asset_ids.len());
        for asset_id in asset_ids {
            if seen.insert(asset_id.as_str()) {
                mutations.push(Self::remove_asset_tag_in_transaction(
                    &transaction,
                    asset_id,
                    &name,
                )?);
            }
        }
        transaction.commit()?;
        Ok(mutations)
    }

    fn remove_asset_tag_in_transaction(
        transaction: &Transaction<'_>,
        asset_id: &str,
        name: &str,
    ) -> CoreResult<TagMutation> {
        let normalized_name = name.to_lowercase();
        Self::ensure_active_asset_in_transaction(transaction, asset_id)?;
        let tag_id = transaction
            .query_row(
                "SELECT id FROM tags WHERE name=?1 COLLATE NOCASE LIMIT 1",
                [&name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let changed = if let Some(tag_id) = tag_id {
            transaction.execute(
                "DELETE FROM asset_tags WHERE asset_id=?1 AND tag_id=?2",
                params![asset_id, tag_id],
            )? > 0
        } else {
            false
        };
        let now = Utc::now().to_rfc3339();
        if changed {
            transaction.execute(
                "UPDATE cards SET status='CardStale',last_checked_at=?2 WHERE asset_id=?1 AND status='CardValid'",
                params![asset_id, now],
            )?;
        }
        transaction.execute(
            "INSERT INTO asset_tag_overrides(asset_id,normalized_name,updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(asset_id,normalized_name) DO UPDATE SET updated_at=excluded.updated_at",
            params![asset_id, normalized_name, now],
        )?;
        Ok(TagMutation {
            asset_id: asset_id.to_owned(),
            name: name.to_owned(),
            source: "user".to_owned(),
            changed,
            blocked_by_user: false,
        })
    }

    pub fn set_favorite(&self, asset_id: &str, favorite: bool) -> CoreResult<bool> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        Self::set_favorite_in_transaction(&transaction, asset_id, favorite)?;
        transaction.commit()?;
        Ok(favorite)
    }

    pub fn set_favorites_batch(&self, asset_ids: &[String], favorite: bool) -> CoreResult<usize> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let transaction = connection.transaction()?;
        let mut seen = HashSet::with_capacity(asset_ids.len());
        let mut changed = 0;
        for asset_id in asset_ids {
            if seen.insert(asset_id.as_str())
                && Self::set_favorite_in_transaction(&transaction, asset_id, favorite)?
            {
                changed += 1;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    fn set_favorite_in_transaction(
        transaction: &Transaction<'_>,
        asset_id: &str,
        favorite: bool,
    ) -> CoreResult<bool> {
        Self::ensure_active_asset_in_transaction(transaction, asset_id)?;
        let was_favorite = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM favorites WHERE asset_id=?1)",
            [asset_id],
            |row| row.get::<_, bool>(0),
        )?;
        if was_favorite == favorite {
            return Ok(false);
        }
        if favorite {
            transaction.execute(
                "INSERT INTO favorites(asset_id,created_at) VALUES (?1,?2) ON CONFLICT(asset_id) DO NOTHING",
                params![asset_id, Utc::now().to_rfc3339()],
            )?;
        } else {
            transaction.execute("DELETE FROM favorites WHERE asset_id=?1", [asset_id])?;
        }
        transaction.execute(
            "UPDATE cards SET status='CardStale',last_checked_at=?2 WHERE asset_id=?1 AND status='CardValid'",
            params![asset_id, Utc::now().to_rfc3339()],
        )?;
        Ok(true)
    }

    pub fn favorite_assets(&self, query: Option<&str>, limit: usize) -> CoreResult<Vec<Asset>> {
        scanner::list_favorites(self, query, limit)
    }

    pub fn rebuild_relations(&self) -> CoreResult<crate::types::RelationRefreshReport> {
        crate::relations::rebuild(self)
    }

    pub fn list_relations(
        &self,
        asset_id: Option<&str>,
        relation_type: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<crate::types::AssetRelation>> {
        crate::relations::list(self, asset_id, relation_type, limit)
    }

    pub fn confirm_relation(&self, relation_id: &str) -> CoreResult<bool> {
        crate::relations::confirm(self, relation_id)
    }

    pub fn list_saved_filters(&self) -> CoreResult<Vec<SavedFilter>> {
        crate::filters::list(self)
    }

    pub fn save_filter(
        &self,
        filter_id: Option<&str>,
        name: &str,
        expression: FilterExpr,
    ) -> CoreResult<SavedFilter> {
        crate::filters::save(self, filter_id, name, expression)
    }

    pub fn remove_saved_filter(&self, filter_id: &str) -> CoreResult<bool> {
        crate::filters::remove(self, filter_id)
    }

    pub fn apply_saved_filter(
        &self,
        filter_id: &str,
        query: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<Asset>> {
        crate::filters::apply(self, filter_id, query, limit)
    }

    pub fn apply_saved_filter_page(
        &self,
        filter_id: &str,
        query: Option<&str>,
        root_id: Option<&str>,
        cursor: Option<&AssetCursor>,
        limit: usize,
    ) -> CoreResult<AssetPage> {
        crate::filters::apply_page(self, filter_id, query, root_id, cursor, limit)
    }

    pub fn list_jobs(&self) -> CoreResult<Vec<serde_json::Value>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
        let mut statement = connection.prepare("SELECT id,asset_id,kind,priority,status,progress,error_json,created_at,updated_at FROM jobs
            ORDER BY CASE WHEN status IN ('Parsing','Rendering','Encoding','Cancelling') THEN 0
                          WHEN status='Pending' THEN 1 ELSE 2 END,
                     priority DESC,created_at DESC LIMIT 100")?;
        let rows = statement.query_map([], |row| {
            let error_json: Option<String> = row.get(6)?;
            Ok(serde_json::json!({
                "id":row.get::<_,String>(0)?, "asset_id":row.get::<_,Option<String>>(1)?, "kind":row.get::<_,String>(2)?,
                "priority":row.get::<_,i64>(3)?, "status":row.get::<_,String>(4)?, "progress":row.get::<_,f64>(5)?,
                "error":error_json.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok()),
                "created_at":row.get::<_,String>(7)?, "updated_at":row.get::<_,String>(8)?
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn job_summary(&self) -> CoreResult<serde_json::Value> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT status,COUNT(*) FROM jobs WHERE kind='thumbnail' GROUP BY status",
        )?;
        let mut counts = serde_json::Map::new();
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
            let (status, count) = row?;
            counts.insert(status, serde_json::json!(count));
        }
        Ok(serde_json::Value::Object(counts))
    }

    pub fn plan_asset_operation(
        &self,
        operation: &str,
        asset_ids: &[String],
        destination_parent: Option<&str>,
        new_name: Option<&str>,
    ) -> CoreResult<crate::operations::AssetOperationPlan> {
        crate::operations::plan(self, operation, asset_ids, destination_parent, new_name)
    }

    pub fn execute_asset_operation(
        &self,
        plan: &crate::operations::AssetOperationPlan,
    ) -> CoreResult<crate::operations::AssetOperationJournalEntry> {
        crate::operations::execute(self, plan)
    }

    pub fn list_operation_journal(
        &self,
        limit: usize,
    ) -> CoreResult<Vec<crate::operations::AssetOperationJournalEntry>> {
        crate::operations::list_journal(self, limit)
    }

    pub fn resolve_operation_journal(&self, operation_id: &str) -> CoreResult<bool> {
        crate::operations::resolve_journal(self, operation_id)
    }

    pub(crate) fn connection(&self) -> CoreResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| CoreError::LockPoisoned)
    }

    pub(crate) fn asset_operation_guard(&self) -> CoreResult<std::sync::MutexGuard<'_, ()>> {
        self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn future_database_is_rejected_before_any_schema_or_journal_change() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-future-db-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("future.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('preserve'); PRAGMA user_version=17;").unwrap();
        }
        assert!(matches!(Library::open(&path), Err(CoreError::UnsupportedDatabaseVersion { found: 17, supported: 16 })));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(), 17);
        assert_eq!(connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0)).unwrap(), "delete");
        assert_eq!(connection.query_row("SELECT value FROM sentinel", [], |row| row.get::<_, String>(0)).unwrap(), "preserve");
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        drop(connection);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn version_fifteen_migration_preserves_existing_task_and_setting() {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch(
            "ALTER TABLE jobs DROP COLUMN claim_owner;
             INSERT INTO settings(key,value_json,updated_at) VALUES ('fixture','42','now');
             INSERT INTO jobs(id,kind,status,created_at,updated_at) VALUES ('old','thumbnail','Failed','now','now');
             PRAGMA user_version=15;"
        ).unwrap();
        library.initialize_schema().unwrap();
        let connection = library.connection().unwrap();
        assert_eq!(connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(), 16);
        assert_eq!(connection.query_row("SELECT value_json FROM settings WHERE key='fixture'", [], |row| row.get::<_, String>(0)).unwrap(), "42");
        assert_eq!(connection.query_row("SELECT status,claim_owner FROM jobs WHERE id='old'", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))).unwrap(), ("Failed".to_owned(), None));
    }

    #[test]
    fn storage_info_uses_the_opened_database_and_its_matching_wal() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-storage-path-{}", Uuid::new_v4()));
        let path = directory.join("日本語-资源.sqlite3");
        let library = Library::open(&path).unwrap();
        let info = library.storage_info().unwrap();
        assert_eq!(info["path"], std::fs::canonicalize(&path).unwrap().to_string_lossy().as_ref());
        assert_eq!(info["databaseBytes"].as_u64().unwrap(), std::fs::metadata(&path).unwrap().len());
        let wal = directory.join("日本語-资源.sqlite3-wal");
        assert_eq!(info["walBytes"].as_u64().unwrap(), std::fs::metadata(wal).unwrap().len());
        assert_eq!(Library::in_memory().unwrap().storage_info().unwrap()["path"], ":memory:");
        drop(library);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn library_browse_filters_and_thumbnail_revision() {
        let library = Library::in_memory().unwrap();
        let current_revision = crate::cards::expected_renderer_revision(&library, AssetType::Model).unwrap();
        {
            let connection = library.connection().unwrap();
            connection.execute_batch(
                "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at)
                 VALUES ('r','model','C:/models','c:/models','Models','now');
                 INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,created_at,updated_at,last_seen_at)
                 VALUES ('a','r','model','A','C:/models/a.pmx','C:/models','now','now','now'),
                        ('b','r','model','B','C:/models/b.pmx','C:/models','now','now','now'),
                        ('c','r','model','C','C:/models/sub/c.pmx','C:/models/sub','now','now','now');
                 INSERT INTO metadata(asset_id,key,value_json) VALUES
                   ('a','parsed','{\"skeleton_class\":\"nonstandard\"}'),
                   ('b','parsed','{\"skeleton_class\":\"standard\"}'),
                   ('c','parsed','{\"skeleton_class\":\"nonstandard\"}');
                 INSERT INTO tags(id,name,created_at) VALUES ('t1','blue','now'),('t2','dress','now');
                 INSERT INTO asset_tags(asset_id,tag_id,source) VALUES
                   ('a','t1','user'),('b','t1','user'),('b','t2','user'),('c','t1','user');
                 INSERT INTO cards(asset_id,card_path,status,last_checked_at,renderer_revision,has_thumbnail)
                   VALUES ('a','unused-a','CardValid','now','old',1);"
            ).unwrap();
            connection.execute("INSERT INTO cards(asset_id,card_path,status,last_checked_at,renderer_revision,has_thumbnail)
                VALUES ('b','unused-b','CardValid','now',?1,1)", [current_revision]).unwrap();
        }
        let rule = |field, value: &str| FilterExpr::Rule { field, operator: crate::FilterOperator::Eq, value: serde_json::json!(value) };
        let expression = FilterExpr::And { children: vec![rule(crate::FilterField::Tag, "blue"), rule(crate::FilterField::SkeletonClass, "nonstandard")] };
        let first = library.list_asset_page_with_filters(Some(AssetType::Model), None, Some("r"), false, None, 1, None, None, true, None, Some(expression.clone())).unwrap();
        assert_eq!(first.items[0].id, "a");
        assert_eq!(first.items[0].metadata["skeleton_class"], "nonstandard");
        let second = library.list_asset_page_with_filters(Some(AssetType::Model), None, Some("r"), false, first.next_cursor.as_ref(), 1, None, None, true, None, Some(expression.clone())).unwrap();
        assert_eq!(second.items[0].id, "c");
        assert!(second.next_cursor.is_none());
        let saved = library.save_filter(None, "Dress", rule(crate::FilterField::Tag, "dress")).unwrap();
        let filtered = library.list_asset_page_with_filters(None, None, None, false, None, 10, None, None, true, Some(&saved.id), Some(expression.clone())).unwrap();
        assert!(filtered.items.is_empty());
        let scoped = library.list_asset_page_with_filters(None, None, None, false, None, 10, None, Some("C:/models"), false, None, Some(expression)).unwrap();
        assert_eq!(scoped.items.len(), 1);
        let multi = FilterExpr::And { children: vec![rule(crate::FilterField::Tag, "blue"), rule(crate::FilterField::Tag, "dress")] };
        let filtered = library.list_asset_page_with_filters(None, None, None, false, None, 10, None, None, true, None, Some(multi)).unwrap();
        assert_eq!(filtered.items[0].id, "b");
        assert_eq!(library.list_tag_names().unwrap(), ["blue", "dress"]);
        let mut progress = Vec::new();
        assert_eq!(library.mark_outdated_thumbnails_with_progress(&mut |item| progress.push(item)).unwrap(), 1);
        assert!(progress.iter().any(|item| item.phase == "检查缩略图版本" && item.completed == Some(2) && item.total == Some(2)));
        assert!(progress.iter().any(|item| item.phase == "标记旧版缩略图" && item.completed == Some(1) && item.total == Some(1)));
        assert_eq!(library.mark_outdated_thumbnails().unwrap(), 0);
        assert_eq!(library.inspect_asset("a").unwrap().card_status, "CardStale");
        assert_eq!(library.inspect_asset("b").unwrap().card_status, "CardValid");
    }

    #[test]
    #[ignore = "requires a disposable database copy at MMDBRIDGE_STARTUP_PROBE_DB"]
    fn copied_database_startup_progress() {
        let path = PathBuf::from(std::env::var_os("MMDBRIDGE_STARTUP_PROBE_DB").unwrap());
        let started = std::time::Instant::now();
        let mut steps = HashSet::new();
        let mut checked_all = false;
        let library = Library::open_with_progress(&path, &mut |progress| {
            steps.insert(progress.step);
            if progress.phase == "检查缩略图版本" && progress.completed == progress.total && progress.total.is_some() { checked_all = true; }
            println!("{}ms {}/6 {} {:?}/{:?}: {}", started.elapsed().as_millis(), progress.step,
                progress.phase, progress.completed, progress.total, progress.detail);
        }).unwrap();
        assert_eq!(steps.len(), 6);
        assert!(checked_all);
        assert!(!library.list_asset_page(None, None, None, false, None, 1, None, None, true).unwrap().items.is_empty());
        println!("ready in {}ms", started.elapsed().as_millis());
    }

    #[test]
    fn removes_legacy_review_markers_without_losing_failure_status() {
        let library = Library::in_memory().unwrap();
        {
            let connection = library.connection().unwrap();
            connection.execute_batch(
                "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at)
                   VALUES ('root','model','C:/models','c:/models','Models','now');
                 INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,statuses_json,created_at,updated_at,last_seen_at)
                   VALUES ('a','root','model','A','C:/models/a.pmx','C:/models','[\"NeedsReview\"]','now','now','now'),
                          ('b','root','model','B','C:/models/b.pmx','C:/models','[\"ParseFailed\",\"NeedsReview\"]','now','now','now');
                 INSERT INTO metadata(asset_id,key,value_json)
                   VALUES ('a','parsed','{\"candidate_reason\":\"old\",\"card_identity_ambiguous\":true,\"vertex_count\":42}');
                 PRAGMA user_version=14;",
            ).unwrap();
        }
        library.initialize_schema().unwrap();
        let connection = library.connection().unwrap();
        let statuses = connection.prepare("SELECT statuses_json FROM assets ORDER BY id").unwrap()
            .query_map([], |row| row.get::<_, String>(0)).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(statuses, ["[\"Ready\"]", "[\"ParseFailed\"]"]);
        let metadata: String = connection.query_row(
            "SELECT value_json FROM metadata WHERE asset_id='a' AND key='parsed'", [], |row| row.get(0),
        ).unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(metadata["vertex_count"], 42);
        assert!(metadata.get("candidate_reason").is_none());
        assert!(metadata.get("card_identity_ambiguous").is_none());
    }

    #[test]
    fn portable_database_applies_file_limits() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-storage-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("library.sqlite3");
        let library = Library::open(&path).unwrap();
        {
            let connection = library.connection().unwrap();
            let page_size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0)).unwrap();
            let max_pages: i64 = connection.pragma_query_value(None, "max_page_count", |row| row.get(0)).unwrap();
            let journal_limit: i64 = connection.pragma_query_value(None, "journal_size_limit", |row| row.get(0)).unwrap();
            assert_eq!(max_pages * page_size, Library::DATABASE_LIMIT_BYTES as i64);
            assert_eq!(journal_limit, Library::WAL_TARGET_BYTES as i64);
        }
        drop(library);
        for name in ["library.sqlite3", "library.sqlite3-wal", "library.sqlite3-shm"] {
            let file = directory.join(name);
            if file.exists() { std::fs::remove_file(file).unwrap(); }
        }
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn non_recursive_root_setting_survives_database_reopen() {
        let directory = std::env::temp_dir().join(format!(
            "mmdbridge-root-setting-{}",
            Uuid::new_v4()
        ));
        let root_path = directory.join("assets");
        std::fs::create_dir_all(&root_path).unwrap();
        let database_path = directory.join("library.sqlite3");
        let library = Library::open(&database_path).unwrap();
        let root = library
            .add_root(AssetType::Model, root_path.to_str().unwrap(), None)
            .unwrap();
        library
            .update_root(&root.id, None, Some(false), None)
            .unwrap();
        drop(library);

        let reopened = Library::open(&database_path).unwrap();
        let stored_root = reopened
            .list_roots()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == root.id)
            .unwrap();

        assert!(!stored_root.scan_recursive);
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

fn validate_tag(name: &str) -> CoreResult<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(CoreError::InvalidTag(
            "tag must contain 1 to 128 UTF-8 bytes and no control characters".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

fn directory_path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn directory_path_equal(left: &Path, right: &Path) -> bool {
    directory_path_key(left) == directory_path_key(right)
}

fn directory_path_within(path: &Path, root: &Path) -> bool {
    let path = directory_path_key(path);
    let root = directory_path_key(root);
    path == root || path.strip_prefix(&root).is_some_and(|suffix| suffix.starts_with('\\'))
}

fn normalize_directory_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => { normalized.pop(); }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn relative_directory_suffix<'a>(parent: &str, directory: &'a str) -> Option<&'a str> {
    let normalized_parent = parent.replace('/', "\\").trim_end_matches('\\').to_owned();
    let prefix = format!("{normalized_parent}\\");
    let candidate = directory.replace('/', "\\");
    let tail = candidate.get(..prefix.len())?;
    if !tail.eq_ignore_ascii_case(&prefix) { return None; }
    directory.get(prefix.len()..)
}

fn validate_tag_request(source: &str, confidence: Option<f64>) -> CoreResult<()> {
    if !matches!(source, "user" | "agent" | "parser") {
        return Err(CoreError::InvalidTag(format!(
            "unsupported tag source: {source}"
        )));
    }
    if confidence.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
        return Err(CoreError::InvalidTag(
            "confidence must be between 0 and 1".to_owned(),
        ));
    }
    Ok(())
}
