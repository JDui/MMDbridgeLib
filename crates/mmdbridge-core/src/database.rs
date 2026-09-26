use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::Utc;
use mmd_anim_format::parse_pmx_model;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::{
    CoreError, CoreResult, scanner,
    thumbnail::GeneratedThumbnail,
    thumbnail_concurrency::ThumbnailConcurrencySettings,
    types::{
        Asset, AssetCursor, AssetPage, AssetTag, AssetType, CardResult, CardValidation, FilterExpr,
        Root, SavedFilter, ScanReport, ScanState, TagMutation,
    },
};

#[derive(Clone)]
pub struct Library {
    connection: Arc<Mutex<Connection>>,
    scan_lock: Arc<Mutex<()>>,
}

impl Library {
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
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
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
            scan_lock: Arc::new(Mutex::new(())),
        };
        library.initialize_schema()?;
        library.thumbnail_concurrency()?;
        Ok(library)
    }

    pub fn in_memory() -> CoreResult<Self> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let library = Self {
            connection: Arc::new(Mutex::new(connection)),
            scan_lock: Arc::new(Mutex::new(())),
        };
        library.initialize_schema()?;
        library.thumbnail_concurrency()?;
        Ok(library)
    }

    pub fn storage_info(&self) -> CoreResult<serde_json::Value> {
        let path = Self::portable_database_path()?;
        let bytes = |path: &Path| std::fs::metadata(path).map(|item| item.len()).unwrap_or(0);
        let wal = path.with_file_name("library.sqlite3-wal");
        Ok(serde_json::json!({
            "path": path,
            "databaseBytes": bytes(&path),
            "walBytes": bytes(&wal),
            "databaseLimitBytes": Self::DATABASE_LIMIT_BYTES,
            "walTargetBytes": Self::WAL_TARGET_BYTES,
        }))
    }

    pub fn compact_storage(&self) -> CoreResult<serde_json::Value> {
        let _scan_guard = self.scan_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
        let connection = self.connection()?;
        let active_scans: i64 = connection.query_row(
            "SELECT COUNT(*) FROM scan_state WHERE status IN ('Pending','Pausing','Discovering','Indexing','Verifying','Relations','Duplicates')",
            [], |row| row.get(0),
        )?;
        let active_cards: i64 = connection.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status IN ('Pending','Parsing','Rendering','Encoding')",
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
        let connection = self
            .connection
            .lock()
            .map_err(|_| CoreError::LockPoisoned)?;
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
             UPDATE roots SET scan_recursive=1 WHERE scan_recursive=0;
             CREATE TABLE IF NOT EXISTS assets (
                 id TEXT PRIMARY KEY, root_id TEXT NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
                 asset_type TEXT NOT NULL, name TEXT NOT NULL, primary_source TEXT NOT NULL,
                 asset_directory TEXT NOT NULL, fingerprint TEXT NOT NULL DEFAULT '', statuses_json TEXT NOT NULL DEFAULT '[]',
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
             INSERT OR IGNORE INTO asset_counts(root_id,asset_type,asset_count)
                 SELECT root_id,asset_type,COUNT(*) FROM assets
                 WHERE NOT EXISTS(SELECT 1 FROM asset_counts) GROUP BY root_id,asset_type;
             INSERT OR IGNORE INTO asset_directory_counts(root_id,directory,asset_count)
                 SELECT root_id,asset_directory,COUNT(*) FROM assets
                 WHERE NOT EXISTS(SELECT 1 FROM asset_directory_counts) GROUP BY root_id,asset_directory;
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
             CREATE TABLE IF NOT EXISTS duplicates (
                 id TEXT PRIMARY KEY, asset_a TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 asset_b TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 similarity REAL NOT NULL, reason_json TEXT NOT NULL, UNIQUE(asset_a, asset_b)
             );
             CREATE INDEX IF NOT EXISTS idx_duplicates_asset_a ON duplicates(asset_a);
             CREATE INDEX IF NOT EXISTS idx_duplicates_asset_b ON duplicates(asset_b);
             CREATE TABLE IF NOT EXISTS versions (
                 family_id TEXT NOT NULL, asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                 version_label TEXT NOT NULL, confidence REAL NOT NULL, reason_json TEXT NOT NULL,
                 PRIMARY KEY(family_id, asset_id)
             );
             CREATE TABLE IF NOT EXISTS cards (
                 asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
                 card_path TEXT NOT NULL, status TEXT NOT NULL, manifest_json TEXT, last_checked_at TEXT NOT NULL
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
             PRAGMA user_version = 9;
             COMMIT;",
        )?;
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
        Ok(())
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

    pub fn add_root(
        &self,
        asset_type: AssetType,
        path: &str,
        display_name: Option<&str>,
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
            "INSERT INTO roots (id, asset_type, path, path_key, display_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, asset_type.as_str(), path_text, path_text.to_lowercase(), name, now],
        )?;
        Ok(Root {
            id,
            asset_type,
            path: path_text,
            display_name: name,
            enabled: true,
            scan_recursive: true,
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
            let connection = self
                .connection
                .lock()
                .map_err(|_| CoreError::LockPoisoned)?;
            connection.execute("DELETE FROM roots WHERE id=?1", [root_id])? > 0
        };
        if removed {
            self.rebuild_relations()?;
            self.rebuild_duplicates()?;
        }
        Ok(removed)
    }

    pub fn scan_root(&self, root_id: &str) -> CoreResult<ScanReport> {
        self.scan_root_with_mode(root_id, false)
    }

    pub(crate) fn scan_queued_root(&self, root_id: &str) -> CoreResult<ScanReport> {
        let full_check = self.connection()?.query_row(
            "SELECT full_check FROM scan_state WHERE root_id=?1", [root_id],
            |row| row.get::<_, i64>(0),
        ).optional()?.unwrap_or(0) != 0;
        self.scan_root_with_mode(root_id, full_check)
    }

    fn scan_root_with_mode(&self, root_id: &str, full_check: bool) -> CoreResult<ScanReport> {
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
        match self.begin_scan(root_id) {
            Ok(()) => {}
            Err(error @ (CoreError::ScanCancelled | CoreError::ScanPaused)) => {
                self.finish_scan(root_id, &Err(error))?;
                return Err(self.scan_interruption(root_id)?);
            }
            Err(error) => return Err(error),
        }
        let result = scanner::scan(self, &root, full_check);
        self.finish_scan(root_id, &result)?;
        result
    }

    pub fn enqueue_scan(&self, root_id: &str) -> CoreResult<ScanState> {
        crate::scan_queue::enqueue(self, root_id, false)
    }

    pub fn enqueue_full_check(&self, root_id: &str) -> CoreResult<ScanState> {
        crate::scan_queue::enqueue(self, root_id, true)
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
             WHERE root_id=?1 AND status IN ('Pending','Paused','Pausing','Discovering','Indexing','Verifying','Relations','Duplicates')",
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
               ('Pending','Discovering','Indexing','Verifying','Relations','Duplicates')",
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
            "SELECT root_id,status,progress,files_seen,files_processed,error_json,updated_at,queue_order,full_check
             FROM scan_state ORDER BY CASE WHEN status='Pending' THEN 0 ELSE 1 END,
             CASE WHEN status='Pending' THEN queue_order END,updated_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let error_json: Option<String> = row.get(5)?;
            Ok(ScanState {
                root_id: row.get(0)?,
                status: row.get(1)?,
                queue_order: row.get(7)?,
                full_check: row.get::<_, i64>(8)? != 0,
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

    pub(crate) fn begin_scan(&self, root_id: &str) -> CoreResult<()> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection()?;
        let changed = connection.execute(
            "INSERT INTO scan_state(root_id,status,progress,error_json,updated_at,files_seen,files_processed)
             VALUES (?1,'Discovering',0,NULL,?2,0,0)
             ON CONFLICT(root_id) DO UPDATE SET status='Discovering',progress=0,error_json=NULL,
               updated_at=excluded.updated_at,files_seen=0,files_processed=0
             WHERE scan_state.status NOT IN ('Cancelling','Paused','Pausing','Cancelled')",
            params![root_id, now],
        )?;
        if changed == 0 { return Err(self.scan_interruption_locked(&connection, root_id)?); }
        connection.execute("UPDATE roots SET scan_status='Scanning' WHERE id=?1", [root_id])?;
        Ok(())
    }

    pub(crate) fn claim_pending_scan(&self, root_id: &str) -> CoreResult<bool> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE scan_state SET status='Discovering',updated_at=?2
             WHERE root_id=?1 AND status='Pending'",
            params![root_id, Utc::now().to_rfc3339()],
        )?;
        Ok(changed > 0)
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

    fn scan_interruption(&self, root_id: &str) -> CoreResult<CoreError> {
        let connection = self.connection()?;
        self.scan_interruption_locked(&connection, root_id)
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

    pub(crate) fn finish_scan(&self, root_id: &str, result: &CoreResult<ScanReport>) -> CoreResult<()> {
        let connection = self.connection()?;
        let now = Utc::now().to_rfc3339();
        match result {
            Ok(report) => {
                let changed = connection.execute("UPDATE scan_state SET status='Completed',progress=1,files_seen=?2,
                    files_processed=?2,error_json=NULL,updated_at=?3
                    WHERE root_id=?1 AND status NOT IN ('Cancelled','Cancelling','Paused','Pausing')",
                    params![root_id, report.files_seen as i64, now])?;
                if changed > 0 {
                    connection.execute("UPDATE roots SET scan_status='Ready',last_scan_at=?2 WHERE id=?1",
                        params![root_id, now])?;
                } else {
                    let paused = matches!(self.scan_interruption_locked(&connection, root_id)?, CoreError::ScanPaused);
                    let status = if paused { "Paused" } else { "Cancelled" };
                    connection.execute("UPDATE scan_state SET status=?2,updated_at=?3 WHERE root_id=?1",
                        params![root_id, status, now])?;
                    connection.execute("UPDATE roots SET scan_status=?2 WHERE id=?1", params![root_id, status])?;
                }
            }
            Err(CoreError::ScanCancelled) => {
                connection.execute("UPDATE scan_state SET status='Cancelled',updated_at=?2 WHERE root_id=?1",
                    params![root_id, now])?;
                connection.execute("UPDATE roots SET scan_status='Cancelled' WHERE id=?1", [root_id])?;
            }
            Err(CoreError::ScanPaused) => {
                let status: Option<String> = connection.query_row(
                    "SELECT status FROM scan_state WHERE root_id=?1", [root_id], |row| row.get(0),
                ).optional()?;
                let final_status = match status.as_deref() {
                    Some("Pausing" | "Paused") => Some("Paused"),
                    Some("Cancelling" | "Cancelled") => Some("Cancelled"),
                    _ => None,
                };
                if let Some(final_status) = final_status {
                    connection.execute("UPDATE scan_state SET status=?2,updated_at=?3 WHERE root_id=?1",
                        params![root_id, final_status, now])?;
                    connection.execute("UPDATE roots SET scan_status=?2 WHERE id=?1",
                        params![root_id, final_status])?;
                }
            }
            Err(error) => {
                connection.execute("UPDATE scan_state SET status='Failed',error_json=?2,updated_at=?3 WHERE root_id=?1",
                    params![root_id, serde_json::json!({"message": error.to_string()}).to_string(), now])?;
                connection.execute("UPDATE roots SET scan_status='Failed' WHERE id=?1", [root_id])?;
            }
        }
        Ok(())
    }

    pub fn model_preview(&self, asset_id: &str) -> CoreResult<Vec<u8>> {
        let asset = self.inspect_asset(asset_id)?;
        if asset.asset_type != AssetType::Model
            || !asset.primary_source.to_ascii_lowercase().ends_with(".pmx")
        {
            return Err(CoreError::ModelPreview(
                "3D preview currently supports PMX model assets".to_owned(),
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
        let bytes = std::fs::read(path)?;
        let parsed =
            parse_pmx_model(&bytes).map_err(|error| CoreError::ModelPreview(error.to_string()))?;
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
        if !model_path.extension().and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("pmx"))
            || texture_path.len() > 4096 {
            return Err(CoreError::ModelPreview("invalid model or texture path".to_owned()));
        }
        let model_bytes = std::fs::read(model_path)?;
        if model_bytes.len() > 512 * 1024 * 1024 {
            return Err(CoreError::ModelPreview("PMX file exceeds the 512 MiB preview limit".to_owned()));
        }
        let parsed = parse_pmx_model(&model_bytes)
            .map_err(|error| CoreError::ModelPreview(error.to_string()))?;
        if !parsed.materials.iter().any(|material| material.texture_path == texture_path) {
            return Err(CoreError::ModelPreview("texture is not referenced by this PMX".to_owned()));
        }
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
            if !source
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
            {
                return Err(CoreError::ThumbnailRender(
                    "动作预览模型必须是 PMX 文件".to_owned(),
                ));
            }
            let metadata = std::fs::metadata(source)?;
            if metadata.len() > 512 * 1024 * 1024 {
                return Err(CoreError::ThumbnailRender(
                    "动作预览模型超过 512 MiB 解析上限".to_owned(),
                ));
            }
            let bytes = std::fs::read(source)?;
            parse_pmx_model(&bytes).map_err(|error| {
                CoreError::ThumbnailRender(format!("动作预览模型 PMX 解析失败：{error}"))
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
        )
    }

    pub fn list_duplicate_asset_page(
        &self,
        asset_type: Option<AssetType>,
        query: Option<&str>,
        root_id: Option<&str>,
        cursor: Option<&AssetCursor>,
        limit: usize,
    ) -> CoreResult<AssetPage> {
        scanner::list_duplicate_asset_page(self, asset_type, query, root_id, cursor, limit)
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
                        "请先在设置中指定 Motion Preview Model（PMX）".to_owned(),
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
                return crate::cards::create(self, asset_id, Some(&preview), Some(&report));
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
            crate::cards::create(
                self,
                asset_id,
                Some(&generated.preview_webp),
                Some(&generated.report),
            )
        } else {
            crate::cards::create(self, asset_id, None, None)
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
                "请先在设置中指定 Motion Preview Model（PMX）".to_owned(),
            ));
        }
        let mut cursor = None;
        let mut job_ids = Vec::new();
        loop {
            let page = self.list_asset_page(Some(root.asset_type), None, Some(root_id), false, cursor.as_ref(), 500, None, None)?;
            for asset in page.items {
                if asset.card_status == "CardValid" && asset.has_thumbnail { continue; }
                if asset.statuses.iter().any(|status| matches!(status.as_str(),
                    "MissingSource" | "ParseFailed" | "Unsupported")) { continue; }
                let extension = Path::new(&asset.primary_source).extension().and_then(|value| value.to_str());
                if !extension.is_some_and(|value| asset.asset_type.supports_thumbnail_extension(value)) { continue; }
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
                "当前渲染器支持 PMX 模型/场景、PMD 和文本 X 场景，以及配置了 Motion Preview Model 的 VMD/VPD 动作".to_owned(),
            ));
        }
        if asset.asset_type == AssetType::Motion {
            let model_path = self.motion_preview_model()?.ok_or_else(|| {
                CoreError::ThumbnailRender(
                    "请先在设置中指定 Motion Preview Model（PMX）".to_owned(),
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

    fn add_asset_tag_in_transaction(
        transaction: &Transaction<'_>,
        asset_id: &str,
        name: &str,
        source: &str,
        confidence: Option<f64>,
    ) -> CoreResult<TagMutation> {
        let normalized_name = name.to_lowercase();
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
            [asset_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(CoreError::AssetNotFound(asset_id.to_owned()));
        }
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
             ON CONFLICT(asset_id,tag_id) DO UPDATE SET source=excluded.source,confidence=excluded.confidence",
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
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
            [asset_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(CoreError::AssetNotFound(asset_id.to_owned()));
        }
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
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
            [asset_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(CoreError::AssetNotFound(asset_id.to_owned()));
        }
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

    pub fn rebuild_duplicates(&self) -> CoreResult<crate::types::DuplicateRefreshReport> {
        crate::duplicates::rebuild(self)
    }

    pub fn list_duplicates(
        &self,
        asset_id: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<crate::types::AssetDuplicate>> {
        crate::duplicates::list(self, asset_id, limit)
    }

    pub fn duplicate_count(&self) -> CoreResult<usize> {
        crate::duplicates::count(self)
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
            ORDER BY CASE WHEN status IN ('Parsing','Rendering','Encoding') THEN 0
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
