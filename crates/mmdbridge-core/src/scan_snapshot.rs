use std::{collections::HashMap, path::Path, time::UNIX_EPOCH};

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{CoreError, CoreResult, Library, Root, ScanChange, ScanChangeKind, ScanState};

#[derive(Serialize, Deserialize)]
struct Snapshot {
    root_key: String,
    recursive: bool,
    asset_type: String,
    parser_revisions: Vec<u32>,
    files: HashMap<String, FileStamp>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct FileStamp {
    path: String,
    // Missing indexed dependencies stay in the snapshot, so their return is detected.
    stat: Option<(u64, u128)>,
}

impl Library {
    /// Reconcile changes made while the app was closed without re-indexing unchanged assets.
    /// Metadata enumeration is linear; no source contents, fingerprints or card archives are read.
    pub fn reconcile_root_changes(&self, root_id: &str) -> CoreResult<Option<ScanState>> {
        let root = self.list_roots()?.into_iter().find(|root| root.id == root_id)
            .ok_or_else(|| CoreError::RootNotFound(root_id.to_owned()))?;
        if !root.enabled { return Err(CoreError::RootDisabled(root_id.to_owned())); }
        let setting_key = format!("scan_snapshot_v1:{root_id}");
        let previous: Option<String> = self.connection()?.query_row(
            "SELECT value_json FROM settings WHERE key=?1", [&setting_key], |row| row.get(0),
        ).optional()?;
        let current = capture(self, &root)?;
        let previous = previous.and_then(|json| serde_json::from_str::<Snapshot>(&json).ok());
        let mut save_snapshot = true;
        let state = if let Some(previous) = previous.filter(|previous| compatible(previous, &current)) {
            save_snapshot = previous.files != current.files;
            let changes = delta(&previous, &current);
            // A failed worker must remain retryable even if its metadata snapshot was saved.
            if root.scan_status == "Failed" || changes.len() > 1024 {
                Some(self.enqueue_scan(root_id)?)
            } else if changes.is_empty() {
                None
            } else {
                self.enqueue_scan_changes(root_id, &changes)?
            }
        } else {
            // First run / changed discovery rules: one baseline discovery, then deltas only.
            Some(self.enqueue_scan(root_id)?)
        };
        // Queue changes durably before advancing the baseline. An interrupted enqueue
        // or unreadable directory must never silently consume a change.
        if save_snapshot {
            self.connection()?.execute(
                "INSERT INTO settings(key,value_json,updated_at) VALUES (?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
                params![setting_key, serde_json::to_string(&current)?, Utc::now().to_rfc3339()],
            )?;
        }
        Ok(state)
    }
}

fn compatible(previous: &Snapshot, current: &Snapshot) -> bool {
    previous.root_key == current.root_key && previous.recursive == current.recursive
        && previous.asset_type == current.asset_type && previous.parser_revisions == current.parser_revisions
}

fn capture(library: &Library, root: &Root) -> CoreResult<Snapshot> {
    let root_path = Path::new(&root.path);
    if !std::fs::metadata(root_path)?.is_dir() {
        return Err(CoreError::InvalidRoot(root.path.clone()));
    }
    let root_key = crate::scanner::scan_path_key(root_path);
    let mut files = HashMap::new();
    // Always include nested dependencies, even for non-recursive primary discovery.
    for entry in WalkDir::new(root_path).follow_links(false) {
        let entry = entry.map_err(|error| std::io::Error::other(error.to_string()))?;
        if !entry.file_type().is_file() { continue; }
        let path = entry.path();
        let primary = root.asset_type.accepts_extension(path.extension().and_then(|ext| ext.to_str()).unwrap_or_default())
            && (root.scan_recursive || path.parent() == Some(root_path));
        if !primary && !crate::scan_queue::is_possible_mmd_dependency(path) { continue; }
        let metadata = entry.metadata().map_err(|error| std::io::Error::other(error.to_string()))?;
        files.insert(crate::scanner::scan_path_key(path), stamp(path, Some(metadata))?);
    }
    let references = {
        let connection = library.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT f.path FROM asset_files f JOIN assets a ON a.id=f.asset_id
             WHERE a.root_id=?1 AND a.retired_format=0
             UNION
             SELECT json_extract(dependency.value,'$.path') FROM assets a
             JOIN metadata m ON m.asset_id=a.id AND m.key='parsed'
             JOIN json_each(CASE WHEN json_valid(m.value_json)
               THEN COALESCE(json_extract(m.value_json,'$.file_dependencies'),'[]') ELSE '[]' END) dependency
             WHERE a.root_id=?1 AND a.retired_format=0
               AND json_extract(dependency.value,'$.status')='missing'
               AND json_extract(dependency.value,'$.path') IS NOT NULL",
        )?;
        statement.query_map([&root.id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for reference in references {
        let path = Path::new(&reference);
        let key = crate::scanner::scan_path_key(path);
        if files.contains_key(&key) { continue; }
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        files.insert(key, stamp(path, metadata)?);
    }
    Ok(Snapshot {
        root_key, recursive: root.scan_recursive, asset_type: root.asset_type.as_str().to_owned(),
        parser_revisions: ["pmx", "pmd", "vmd", "vpd"].map(|ext| crate::parser::revision_for_path(Path::new(&format!("asset.{ext}")))).to_vec(),
        files,
    })
}

fn stamp(path: &Path, metadata: Option<std::fs::Metadata>) -> CoreResult<FileStamp> {
    let stat = metadata.map(|metadata| Ok::<_, std::io::Error>((metadata.len(),
        metadata.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())))
        .transpose()?;
    Ok(FileStamp { path: path.to_string_lossy().into_owned(), stat })
}

fn delta(previous: &Snapshot, current: &Snapshot) -> Vec<ScanChange> {
    let mut changes = Vec::new();
    for (key, file) in &current.files {
        if previous.files.get(key).is_some_and(|old| old.stat == file.stat) { continue; }
        if !within(key, &current.root_key) {
            // The existing queue deliberately rejects out-of-root file events.
            return vec![ScanChange { path: String::new(), kind: ScanChangeKind::Root }];
        }
        if file.stat.is_some() || previous.files.get(key).is_some_and(|old| old.stat.is_some()) {
            changes.push(ScanChange { path: file.path.clone(), kind: if file.stat.is_some() { ScanChangeKind::File } else { ScanChangeKind::Removed } });
        }
    }
    for (key, file) in &previous.files {
        if current.files.contains_key(key) || file.stat.is_none() { continue; }
        if !within(key, &current.root_key) {
            return vec![ScanChange { path: String::new(), kind: ScanChangeKind::Root }];
        }
        changes.push(ScanChange { path: file.path.clone(), kind: ScanChangeKind::Removed });
    }
    changes
}

fn within(key: &str, root: &str) -> bool {
    key == root || key.strip_prefix(root).is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_snapshot_detects_offline_changes_and_preserves_failed_capture() {
        let temp = std::env::temp_dir().join(format!("mmdbridge-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp.join("素材/贴图")).unwrap();
        let source = temp.join("素材/model.pmx");
        let texture = temp.join("素材/贴图/skin.png");
        std::fs::write(&source, b"model").unwrap();
        std::fs::write(&texture, b"texture").unwrap();
        let library = Library::in_memory().unwrap();
        let root = library.add_root(crate::AssetType::Model, &temp.join("素材").to_string_lossy(), None).unwrap();
        // Pause the queue so the fixture does not parse or write a resource card.
        library.connection().unwrap().execute(
            "INSERT INTO scan_state(root_id,status,updated_at) VALUES (?1,'Paused','now')", [&root.id],
        ).unwrap();
        library.reconcile_root_changes(&root.id).unwrap();
        library.connection().unwrap().execute("DELETE FROM scan_changes WHERE root_id=?1", [&root.id]).unwrap();
        assert!(library.reconcile_root_changes(&root.id).unwrap().is_none());
        assert_eq!(library.list_scan_states().unwrap()[0].status, "Paused");
        let initial = capture(&library, &root).unwrap();
        std::fs::write(&texture, b"changed texture").unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::write(temp.join("素材/新模型.pmx"), b"new").unwrap();
        let current = capture(&library, &root).unwrap();
        let changes = delta(&initial, &current);
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().any(|change| Path::new(&change.path).file_name() == source.file_name() && change.kind == ScanChangeKind::Removed), "{changes:?}");
        library.reconcile_root_changes(&root.id).unwrap();
        assert!(library.reconcile_root_changes(&root.id).unwrap().is_none());
        let saved: String = library.connection().unwrap().query_row("SELECT value_json FROM settings WHERE key=?1", [format!("scan_snapshot_v1:{}", root.id)], |row| row.get(0)).unwrap();
        std::fs::rename(temp.join("素材"), temp.join("离线")).unwrap();
        assert!(library.reconcile_root_changes(&root.id).is_err());
        let retained: String = library.connection().unwrap().query_row("SELECT value_json FROM settings WHERE key=?1", [format!("scan_snapshot_v1:{}", root.id)], |row| row.get(0)).unwrap();
        assert_eq!(saved, retained);
        std::fs::remove_dir_all(&temp).unwrap();
    }
}
