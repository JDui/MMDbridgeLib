use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::Utc;
use rusqlite::{OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    CoreError, CoreResult, Library, cards,
    parser::parse_asset,
    types::{
        Asset, AssetCursor, AssetPage, AssetType, ParsedCandidate, Root, ScanReport, display_name,
    },
};

pub(crate) const MAX_LIST_ITEMS: usize = 50_000;
const ASSET_SEARCH_PREDICATE: &str =
    "(?1 IS NULL OR NOT EXISTS(
        SELECT 1 FROM json_each(?1) search_term
        WHERE NOT EXISTS (
            SELECT 1 FROM json_each(search_term.value) search_variant
            WHERE (
                a.name LIKE search_variant.value ESCAPE '\\' COLLATE NOCASE
                OR a.primary_source LIKE search_variant.value ESCAPE '\\' COLLATE NOCASE
                OR a.asset_directory LIKE search_variant.value ESCAPE '\\' COLLATE NOCASE
                OR EXISTS(SELECT 1 FROM asset_tags at JOIN tags t ON t.id=at.tag_id
                          WHERE at.asset_id=a.id AND t.name LIKE search_variant.value ESCAPE '\\' COLLATE NOCASE)
            )
        )
    ))";

pub(crate) fn scan(library: &Library, root: &Root, full_check: bool) -> CoreResult<ScanReport> {
    let root_path = Path::new(&root.path);
    let root_path_key = scan_path_key(root_path);
    let walker = if root.scan_recursive {
        WalkDir::new(root_path)
    } else {
        WalkDir::new(root_path).max_depth(1)
    };
    let mut files = Vec::new();
    let mut unsupported = 0;
    let mut visited = 0_usize;
    for entry in walker {
        let entry = entry.map_err(|error| {
            std::io::Error::other(format!("failed to walk {}: {error}", root.path))
        })?;
        visited += 1;
        if visited % 1024 == 0 {
            library.update_scan_progress(&root.id, "Discovering", 0.02, files.len(), 0)?;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if root.asset_type.accepts_extension(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default(),
        ) {
            files.push(path.to_path_buf());
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ["pmx", "pmd", "vmd", "vpd", "x"]
                    .iter()
                    .any(|supported| ext.eq_ignore_ascii_case(supported))
            })
        {
            unsupported += 1;
        }
    }
    files.sort_by(|left, right| {
        left.to_string_lossy()
            .to_lowercase()
            .cmp(&right.to_string_lossy().to_lowercase())
    });
    let current_paths = files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<HashSet<_>>();
    let multiple_pmx_dirs = multiple_pmx_directories(&files, root.asset_type);
    let files_seen = files.len();
    library.update_scan_progress(&root.id, "Indexing", 0.1, files_seen, 0)?;
    let now = Utc::now().to_rfc3339();
    let mut report = ScanReport {
        root_id: root.id.clone(),
        files_seen: files.len(),
        assets_added: 0,
        assets_updated: 0,
        assets_unchanged: 0,
        parse_failures: 0,
        unsupported_files: unsupported,
        missing_sources: 0,
        completed_at: now.clone(),
    };
    let mut seen_paths = HashSet::new();
    for (index, path) in files.into_iter().enumerate() {
        if index % 32 == 0 || index + 1 == files_seen {
            library.update_scan_progress(
                &root.id, "Indexing", 0.1 + 0.72 * index as f64 / files_seen.max(1) as f64,
                files_seen, index,
            )?;
        }
        let path_text = path.to_string_lossy().into_owned();
        seen_paths.insert(path_text.clone());
        let filesystem_metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let file_size = i64::try_from(filesystem_metadata.len()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("file is larger than SQLite supports: {}", path.display()),
            )
        })?;
        let modified_ns = filesystem_metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0_i64, |duration| {
                i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
            });
        let existing: Option<(String, i64, i64, String, String)> = {
            let connection = library.connection()?;
            connection.query_row(
                "SELECT a.id,f.file_size,f.modified_ns,a.statuses_json,a.fingerprint FROM assets a JOIN asset_files f ON f.asset_id=a.id WHERE a.root_id=?1 AND f.path=?2 AND f.role='primary' LIMIT 1",
                params![root.id, path_text], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).optional()?
        };
        let checked_bytes = full_check.then(|| read_bytes(&path));
        let checked_fingerprint = checked_bytes.as_ref().and_then(|result| result.as_ref().ok())
            .map(|bytes| format!("blake3:{}", blake3::hash(bytes).to_hex()));
        if let Some((asset_id, old_size, old_modified, old_statuses, old_fingerprint)) = &existing {
            let previously_unsupported_binary_x = parse_statuses(old_statuses)
                .iter()
                .any(|status| status == "Unsupported")
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("x"))
                && is_binary_x_file(&path).unwrap_or(true);
            if *old_size == file_size
                && *old_modified == modified_ns
                && (!full_check || checked_fingerprint.as_deref() == Some(old_fingerprint.as_str()))
                && !previously_unsupported_binary_x
                && !parse_statuses(old_statuses)
                    .iter()
                    .any(|status| status == "ParseFailed")
                && dependencies_unchanged(library, asset_id)?
            {
                let statuses = parse_statuses(old_statuses)
                    .into_iter()
                    .filter(|status| status != "MissingSource")
                    .collect::<Vec<_>>();
                let connection = library.connection()?;
                connection.execute(
                    "UPDATE assets SET statuses_json=?2,last_seen_at=?3 WHERE id=?1",
                    params![asset_id, serde_json::to_string(&statuses)?, now],
                )?;
                report.assets_unchanged += 1;
                continue;
            }
        }

        let bytes = match checked_bytes.unwrap_or_else(|| read_bytes(&path)) {
            Ok(bytes) => bytes,
            Err(message) => {
                report.parse_failures += 1;
                let mut parsed = ParsedCandidate {
                    name: display_name(&path),
                    metadata: serde_json::json!({"error":{"error_code":"ReadFailed","message":message,"source":path_text,"recoverable":true}}),
                    status: "ParseFailed".to_owned(),
                    dependencies: Vec::new(),
                };
                if multiple_pmx_dirs.contains(path.parent().unwrap_or(root_path))
                    && root.asset_type != AssetType::Motion
                {
                    mark_multiple_pmx_issue(&mut parsed);
                }
                store_candidate(
                    library,
                    root,
                    &path,
                    file_size,
                    modified_ns,
                    String::new(),
                    parsed,
                    existing.as_ref().map(|v| v.0.as_str()),
                    None,
                    &now,
                )?;
                continue;
            }
        };
        let fingerprint = format!("blake3:{}", blake3::hash(&bytes).to_hex());
        let moved_id = if existing.is_none() {
            find_moved_asset(library, root, &fingerprint, &current_paths)?
        } else {
            None
        };
        let mut parsed = match parse_asset(root.asset_type, &path, &bytes) {
            Ok(parsed) => parsed,
            Err(message) => {
                report.parse_failures += 1;
                ParsedCandidate {
                    name: display_name(&path),
                    metadata: serde_json::json!({
                        "file_type":path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase(),
                        "error":{"error_code":"ParseFailed","message":message,"source":path_text,"recoverable":false}
                    }),
                    status: "ParseFailed".to_owned(),
                    dependencies: Vec::new(),
                }
            }
        };
        resolve_dependencies(&path, &mut parsed);
        if multiple_pmx_dirs.contains(path.parent().unwrap_or(root_path))
            && root.asset_type != AssetType::Motion
        {
            mark_multiple_pmx_issue(&mut parsed);
        }
        let relative_path = path
            .strip_prefix(root_path)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let nearby_identity = if existing.is_none() {
            cards::find_nearby_identity(
                &path,
                &parsed.name,
                root.asset_type,
                &relative_path,
                &fingerprint,
            )
        } else {
            cards::NearbyCardIdentity::None
        };
        let (
            mut recovered_id,
            mut card_path_hint,
            mut recovered_tags,
            mut recovered_suppressed_tags,
            mut recovered_favorite,
            mut ambiguous_identity,
        ) = match nearby_identity {
            cards::NearbyCardIdentity::None => (None, None, Vec::new(), Vec::new(), false, false),
            cards::NearbyCardIdentity::Unique(identity) => {
                if moved_id
                    .as_deref()
                    .is_some_and(|moved| moved != identity.asset_id)
                {
                    (None, None, Vec::new(), Vec::new(), false, true)
                } else {
                    (
                        Some(identity.asset_id),
                        Some(identity.card_path),
                        identity.tags,
                        identity.suppressed_tags,
                        identity.favorite,
                        false,
                    )
                }
            }
            cards::NearbyCardIdentity::Ambiguous => {
                (None, None, Vec::new(), Vec::new(), false, true)
            }
        };
        if let Some(card_id) = recovered_id.as_deref() {
            let already_indexed = library.connection()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
                [card_id],
                |row| row.get::<_, bool>(0),
            )?;
            if already_indexed && moved_id.as_deref() != Some(card_id) {
                recovered_id = None;
                card_path_hint = None;
                recovered_tags.clear();
                recovered_suppressed_tags.clear();
                recovered_favorite = false;
                ambiguous_identity = true;
            }
        }
        if ambiguous_identity {
            mark_candidate_needs_review(&mut parsed);
            if let Value::Object(metadata) = &mut parsed.metadata {
                metadata.insert("card_identity_ambiguous".to_owned(), Value::Bool(true));
            }
        }
        let selected_id = existing
            .as_ref()
            .map(|value| value.0.as_str())
            .or(moved_id.as_deref())
            .or(recovered_id.as_deref());
        let restore_card_path = card_path_hint
            .as_deref()
            .filter(|_| recovered_id.as_deref() == selected_id);
        if !store_candidate(
            library,
            root,
            &path,
            file_size,
            modified_ns,
            fingerprint,
            parsed,
            selected_id,
            restore_card_path,
            &now,
        )? {
            report.parse_failures += 1;
            continue;
        }
        if let Some(restored_asset_id) = recovered_id.as_deref() {
            for tag in recovered_tags {
                library.add_asset_tag(restored_asset_id, &tag, "user", None)?;
            }
            for tag in recovered_suppressed_tags {
                library.remove_asset_tag(restored_asset_id, &tag)?;
            }
            if recovered_favorite {
                library.set_favorite(restored_asset_id, true)?;
            }
        }
        if existing.is_some() || moved_id.is_some() || recovered_id.is_some() {
            report.assets_updated += 1;
        } else {
            report.assets_added += 1;
        }
    }

    {
        let connection = library.connection()?;
        let mut statement = connection
            .prepare("SELECT id,primary_source,statuses_json FROM assets WHERE root_id=?1")?;
        let rows = statement.query_map([&root.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let records = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (asset_id, source, statuses_json) in records {
            let in_scan_scope = root.scan_recursive
                || Path::new(&source)
                    .parent()
                    .is_some_and(|parent| scan_path_key(parent) == root_path_key);
            if !in_scan_scope {
                continue;
            }
            let mut statuses = parse_statuses(&statuses_json);
            if seen_paths.contains(&source) {
                statuses.retain(|status| status != "MissingSource");
            } else {
                if !statuses.iter().any(|status| status == "MissingSource") {
                    statuses.push("MissingSource".to_owned());
                }
                report.missing_sources += 1;
            }
            connection.execute(
                "UPDATE assets SET statuses_json=?2 WHERE id=?1",
                params![asset_id, serde_json::to_string(&statuses)?],
            )?;
        }
    }

    library.update_scan_progress(&root.id, "Verifying", 0.83, files_seen, files_seen)?;
    let asset_ids = {
        let connection = library.connection()?;
        let mut statement = connection.prepare("SELECT id FROM assets WHERE root_id=?1")?;
        statement
            .query_map([&root.id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let verify_total = asset_ids.len();
    for (index, asset_id) in asset_ids.into_iter().enumerate() {
        if index % 32 == 0 {
            library.update_scan_progress(
                &root.id, "Verifying", 0.83 + 0.12 * index as f64 / verify_total.max(1) as f64,
                files_seen, files_seen,
            )?;
        }
        library.verify_card(&asset_id)?;
    }
    library.update_scan_progress(&root.id, "Relations", 0.95, files_seen, files_seen)?;
    library.rebuild_relations()?;
    library.update_scan_progress(&root.id, "Duplicates", 0.975, files_seen, files_seen)?;
    library.rebuild_duplicates()?;
    library.update_scan_progress(&root.id, "Duplicates", 0.99, files_seen, files_seen)?;
    Ok(report)
}

pub(crate) fn list_assets(
    library: &Library,
    asset_type: Option<AssetType>,
    query: Option<&str>,
    limit: usize,
) -> CoreResult<Vec<Asset>> {
    list_assets_filtered(library, asset_type, query, limit, false)
}

pub(crate) fn inspect_asset(library: &Library, asset_id: &str) -> CoreResult<Asset> {
    list_assets_with_predicate(
        library,
        None,
        1,
        "a.id=?2",
        vec![SqlValue::Text(asset_id.to_owned())],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| CoreError::AssetNotFound(asset_id.to_owned()))
}

pub(crate) fn list_favorites(
    library: &Library,
    query: Option<&str>,
    limit: usize,
) -> CoreResult<Vec<Asset>> {
    list_assets_filtered(library, None, query, limit, true)
}

pub(crate) fn list_asset_page(
    library: &Library,
    asset_type: Option<AssetType>,
    query: Option<&str>,
    root_id: Option<&str>,
    favorite_only: bool,
    cursor: Option<&AssetCursor>,
    limit: usize,
    motion_format: Option<&str>,
    directory_path: Option<&str>,
) -> CoreResult<AssetPage> {
    let type_text = asset_type.map(|asset_type| asset_type.as_str().to_owned());
    list_asset_page_with_predicate(
        library,
        query,
        root_id,
        limit,
        "(?2 IS NULL OR a.asset_type=?2) AND (?3=0 OR EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id))
         AND (?4 IS NULL OR (a.asset_type='motion' AND lower(a.primary_source) LIKE ('%.' || ?4)))
         AND (?5 IS NULL OR a.asset_directory=?5 OR substr(a.asset_directory,1,length(?5)+1)=?5 || char(92))",
        vec![
            type_text.map(SqlValue::Text).unwrap_or(SqlValue::Null),
            SqlValue::Integer(if favorite_only { 1 } else { 0 }),
            motion_format.map(|value| SqlValue::Text(value.to_owned())).unwrap_or(SqlValue::Null),
            directory_path.map(|value| SqlValue::Text(value.to_owned())).unwrap_or(SqlValue::Null),
        ],
        cursor,
    )
}

pub(crate) fn list_duplicate_asset_page(
    library: &Library,
    asset_type: Option<AssetType>,
    query: Option<&str>,
    root_id: Option<&str>,
    cursor: Option<&AssetCursor>,
    limit: usize,
) -> CoreResult<AssetPage> {
    let type_text = asset_type.map(|asset_type| asset_type.as_str().to_owned());
    list_asset_page_with_predicate(
        library,
        query,
        root_id,
        limit,
        "EXISTS(SELECT 1 FROM duplicates d WHERE d.asset_a=a.id OR d.asset_b=a.id)
         AND (?2 IS NULL OR a.asset_type=?2)",
        vec![type_text.map(SqlValue::Text).unwrap_or(SqlValue::Null)],
        cursor,
    )
}

pub(crate) fn list_assets_with_predicate(
    library: &Library,
    query: Option<&str>,
    limit: usize,
    predicate: &str,
    filter_values: Vec<SqlValue>,
) -> CoreResult<Vec<Asset>> {
    let limit_parameter = filter_values.len() + 2;
    let sql = format!(
        "SELECT a.id,a.asset_type,a.root_id,a.name,a.primary_source,a.asset_directory,a.fingerprint,a.statuses_json,a.updated_at,m.value_json,COALESCE(c.status,'CardMissing'),c.manifest_json,
                EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id),
                (SELECT camera.primary_source FROM relations r JOIN assets camera ON camera.id=r.target_asset
                 WHERE r.relation_type='MotionCameraPair' AND r.source_asset=a.id
                   AND camera.asset_directory=a.asset_directory ORDER BY r.confidence DESC LIMIT 1)
         FROM assets a LEFT JOIN metadata m ON m.asset_id=a.id AND m.key='parsed' LEFT JOIN cards c ON c.asset_id=a.id
         WHERE {ASSET_SEARCH_PREDICATE}
           AND ({predicate})
         ORDER BY a.name COLLATE NOCASE LIMIT ?{limit_parameter}"
    );
    let mut values = Vec::with_capacity(filter_values.len() + 2);
    values.push(
        query
            .and_then(search_query_parameter)
            .map(SqlValue::Text)
            .unwrap_or(SqlValue::Null),
    );
    values.extend(filter_values);
    values.push(SqlValue::Integer(
        i64::try_from(limit.min(MAX_LIST_ITEMS)).unwrap_or(MAX_LIST_ITEMS as i64),
    ));
    let connection = library.connection()?;
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), asset_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn list_asset_page_with_predicate(
    library: &Library,
    query: Option<&str>,
    root_id: Option<&str>,
    limit: usize,
    predicate: &str,
    filter_values: Vec<SqlValue>,
    cursor: Option<&AssetCursor>,
) -> CoreResult<AssetPage> {
    let page_size = limit.clamp(1, MAX_LIST_ITEMS);
    let root_parameter = filter_values.len() + 2;
    let cursor_name_parameter = root_parameter + 1;
    let cursor_id_parameter = root_parameter + 2;
    let limit_parameter = root_parameter + 3;
    let sql = format!(
        "SELECT a.id,a.asset_type,a.root_id,a.name,a.primary_source,a.asset_directory,a.fingerprint,a.statuses_json,a.updated_at,m.value_json,COALESCE(c.status,'CardMissing'),c.manifest_json,
                EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id),
                (SELECT camera.primary_source FROM relations r JOIN assets camera ON camera.id=r.target_asset
                 WHERE r.relation_type='MotionCameraPair' AND r.source_asset=a.id
                   AND camera.asset_directory=a.asset_directory ORDER BY r.confidence DESC LIMIT 1)
         FROM assets a LEFT JOIN metadata m ON m.asset_id=a.id AND m.key='parsed' LEFT JOIN cards c ON c.asset_id=a.id
         WHERE {ASSET_SEARCH_PREDICATE}
           AND ({predicate})
           AND (?{root_parameter} IS NULL OR a.root_id=?{root_parameter})
           AND (?{cursor_name_parameter} IS NULL OR a.name COLLATE NOCASE > ?{cursor_name_parameter} COLLATE NOCASE
                OR (a.name COLLATE NOCASE = ?{cursor_name_parameter} COLLATE NOCASE AND a.id > ?{cursor_id_parameter}))
         ORDER BY a.name COLLATE NOCASE,a.id LIMIT ?{limit_parameter}"
    );
    let mut values = Vec::with_capacity(filter_values.len() + 5);
    values.push(
        query
            .and_then(search_query_parameter)
            .map(SqlValue::Text)
            .unwrap_or(SqlValue::Null),
    );
    values.extend(filter_values);
    values.push(
        root_id
            .map(str::to_owned)
            .map(SqlValue::Text)
            .unwrap_or(SqlValue::Null),
    );
    values.push(
        cursor
            .map(|cursor| SqlValue::Text(cursor.name.clone()))
            .unwrap_or(SqlValue::Null),
    );
    values.push(
        cursor
            .map(|cursor| SqlValue::Text(cursor.id.clone()))
            .unwrap_or(SqlValue::Null),
    );
    values.push(SqlValue::Integer((page_size + 1) as i64));

    let connection = library.connection()?;
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), asset_from_row)?;
    let mut items = rows.collect::<Result<Vec<_>, _>>()?;
    let has_more = items.len() > page_size;
    if has_more {
        items.truncate(page_size);
    }
    let next_cursor = if has_more {
        items.last().map(|asset| AssetCursor {
            name: asset.name.clone(),
            id: asset.id.clone(),
        })
    } else {
        None
    };
    Ok(AssetPage { items, next_cursor })
}

fn list_assets_filtered(
    library: &Library,
    asset_type: Option<AssetType>,
    query: Option<&str>,
    limit: usize,
    favorite_only: bool,
) -> CoreResult<Vec<Asset>> {
    let type_text = asset_type.map(|asset_type| asset_type.as_str().to_owned());
    list_assets_with_predicate(
        library,
        query,
        limit,
        "(?2 IS NULL OR a.asset_type=?2) AND (?3=0 OR EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id))",
        vec![
            type_text.map(SqlValue::Text).unwrap_or(SqlValue::Null),
            SqlValue::Integer(i64::from(favorite_only)),
        ],
    )
}

fn search_query_parameter(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .map(|term| {
            let forms = [
                term.nfc().collect::<String>(),
                term.nfd().collect::<String>(),
            ];
            let mut patterns = Vec::with_capacity(2);
            for form in forms {
                let pattern = Value::String(format!(
                    "%{}%",
                    form.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                ));
                if !patterns.contains(&pattern) {
                    patterns.push(pattern);
                }
            }
            Value::Array(patterns)
        })
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| Value::Array(terms).to_string())
}

fn asset_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let raw_type: String = row.get(1)?;
    let preview_settings_version = if raw_type == "scene" {
        crate::thumbnail::SCENE_PREVIEW_SETTINGS_VERSION
    } else {
        crate::thumbnail::PREVIEW_SETTINGS_VERSION
    };
    let statuses_json: String = row.get(7)?;
    let metadata_json: Option<String> = row.get(9)?;
    let manifest_json: Option<String> = row.get(11)?;
    let thumbnail = manifest_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|manifest| manifest.get("thumbnail").cloned());
    let has_thumbnail = thumbnail.as_ref().is_some_and(|thumbnail| {
        !thumbnail.is_null()
            && thumbnail.pointer("/render_report/rendererVersion").and_then(Value::as_str)
                == Some(crate::thumbnail::RENDERER_VERSION)
            && thumbnail.pointer("/render_report/previewSettingsVersion").and_then(Value::as_str)
                .is_some_and(|version| version.starts_with(preview_settings_version))
    });
    let card_status: String = row.get(10)?;
    let card_status = if thumbnail.is_some_and(|thumbnail| !thumbnail.is_null())
        && !has_thumbnail && card_status == "CardValid" {
        "CardStale".to_owned()
    } else { card_status };
    let mut metadata = metadata_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .unwrap_or(Value::Null);
    let paired_camera: Option<String> = row.get(13)?;
    if let (Some(path), Some(object)) = (paired_camera, metadata.as_object_mut()) {
        object.insert("paired_camera_path".to_owned(), Value::String(path));
    }
    Ok(Asset {
        id: row.get(0)?,
        asset_type: AssetType::parse(&raw_type).unwrap_or(AssetType::Model),
        root_id: row.get(2)?,
        name: row.get(3)?,
        primary_source: row.get(4)?,
        asset_directory: row.get(5)?,
        fingerprint: row.get(6)?,
        metadata,
        statuses: parse_statuses(&statuses_json),
        updated_at: row.get(8)?,
        card_status,
        has_thumbnail,
        is_favorite: row.get(12)?,
    })
}

fn store_candidate(
    library: &Library,
    root: &Root,
    path: &Path,
    file_size: i64,
    modified_ns: i64,
    fingerprint: String,
    parsed: ParsedCandidate,
    existing_id: Option<&str>,
    card_path_hint: Option<&Path>,
    now: &str,
) -> CoreResult<bool> {
    let path_text = path.to_string_lossy().into_owned();
    let id = existing_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let statuses = serde_json::to_string(&candidate_statuses(&parsed))?;
    let asset_directory = path
        .parent()
        .unwrap_or(Path::new(&root.path))
        .to_string_lossy()
        .into_owned();
    let parsed_metadata = serde_json::to_string(&parsed.metadata)?;
    let mut dependency_files = Vec::new();
    for dependency in parsed.dependencies.iter().filter(|dependency| {
        dependency.status == "resolved" && dependency.path.as_deref() != Some(path_text.as_str())
    }) {
        let Some(dependency_path) = dependency.path.as_deref() else {
            return Ok(false);
        };
        let metadata = match std::fs::metadata(dependency_path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Ok(false),
            Err(_) => return Ok(false),
        };
        dependency_files.push((
            dependency_path.to_owned(),
            dependency.role.as_str(),
            i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            metadata_modified_ns(&metadata),
        ));
    }
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    let existing_row = if existing_id.is_some() {
        transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id=?1)",
            [id.as_str()],
            |row| row.get::<_, bool>(0),
        )?
    } else {
        false
    };
    let old_source = if existing_row {
        transaction
            .query_row(
                "SELECT primary_source FROM assets WHERE id=?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    } else {
        None
    };
    let card_path = if let Some(card_path_hint) = card_path_hint {
        card_path_hint.to_string_lossy().into_owned()
    } else if old_source.as_deref() == Some(path_text.as_str()) {
        transaction
            .query_row(
                "SELECT card_path FROM cards WHERE asset_id=?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| {
                cards::default_card_path(path, &parsed.name)
                    .to_string_lossy()
                    .into_owned()
            })
    } else {
        cards::default_card_path(path, &parsed.name)
            .to_string_lossy()
            .into_owned()
    };
    if existing_row {
        transaction.execute(
            "UPDATE assets SET root_id=?2,asset_type=?3,name=?4,primary_source=?5,asset_directory=?6,fingerprint=?7,statuses_json=?8,updated_at=?9,last_seen_at=?9 WHERE id=?1",
            params![id, root.id, root.asset_type.as_str(), parsed.name, path_text, asset_directory, fingerprint, statuses, now],
        )?;
        transaction.execute("DELETE FROM asset_files WHERE asset_id=?1", [&id])?;
    } else {
        transaction.execute(
            "INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,statuses_json,created_at,updated_at,last_seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?9)
             ON CONFLICT(root_id,primary_source) DO UPDATE SET name=excluded.name,asset_directory=excluded.asset_directory,fingerprint=excluded.fingerprint,statuses_json=excluded.statuses_json,updated_at=excluded.updated_at,last_seen_at=excluded.last_seen_at",
            params![id, root.id, root.asset_type.as_str(), parsed.name, path_text, asset_directory, fingerprint, statuses, now],
        )?;
    }
    let stored_id: String = transaction.query_row(
        "SELECT id FROM assets WHERE root_id=?1 AND primary_source=?2",
        params![root.id, path_text],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO asset_files(asset_id,path,role,file_size,modified_ns) VALUES (?1,?2,'primary',?3,?4)
         ON CONFLICT(asset_id,path) DO UPDATE SET file_size=excluded.file_size,modified_ns=excluded.modified_ns",
        params![stored_id, path_text, file_size, modified_ns],
    )?;
    for (dependency_path, role, dependency_size, dependency_modified) in dependency_files {
        transaction.execute(
            "INSERT INTO asset_files(asset_id,path,role,file_size,modified_ns) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(asset_id,path) DO UPDATE SET role=excluded.role,file_size=excluded.file_size,modified_ns=excluded.modified_ns",
            params![stored_id, dependency_path, role, dependency_size, dependency_modified],
        )?;
    }
    transaction.execute("INSERT INTO metadata(asset_id,key,value_json) VALUES (?1,'parsed',?2) ON CONFLICT(asset_id,key) DO UPDATE SET value_json=excluded.value_json", params![stored_id, parsed_metadata])?;
    transaction.execute("INSERT INTO cards(asset_id,card_path,status,last_checked_at) VALUES (?1,?2,'CardMissing',?3) ON CONFLICT(asset_id) DO UPDATE SET card_path=excluded.card_path,status=CASE WHEN cards.status='CardValid' THEN 'CardStale' ELSE cards.status END,last_checked_at=excluded.last_checked_at", params![stored_id, card_path, now])?;
    transaction.commit()?;
    Ok(true)
}

fn dependencies_unchanged(library: &Library, asset_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    let mut indexed_paths = HashSet::new();
    let mut statement = connection.prepare(
        "SELECT path,file_size,modified_ns FROM asset_files WHERE asset_id=?1 AND role<>'primary'",
    )?;
    let rows = statement.query_map([asset_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (path, old_size, old_modified) = row?;
        indexed_paths.insert(path.clone());
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(false);
        };
        if !metadata.is_file()
            || i64::try_from(metadata.len()).unwrap_or(i64::MAX) != old_size
            || metadata_modified_ns(&metadata) != old_modified
        {
            return Ok(false);
        }
    }
    drop(statement);
    let primary_source: String = connection.query_row(
        "SELECT primary_source FROM assets WHERE id=?1",
        [asset_id],
        |row| row.get(0),
    )?;
    indexed_paths.insert(primary_source);

    let parsed_json: Option<String> = connection
        .query_row(
            "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
            [asset_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(parsed_json) = parsed_json else {
        return Ok(false);
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&parsed_json) else {
        return Ok(false);
    };
    let Some(dependencies) = parsed.get("file_dependencies").and_then(Value::as_array) else {
        return Ok(false);
    };
    for dependency in dependencies {
        let status = dependency.get("status").and_then(Value::as_str);
        let path = dependency.get("path").and_then(Value::as_str);
        if status == Some("resolved") && path.is_none_or(|path| !indexed_paths.contains(path)) {
            return Ok(false);
        }
        if status == Some("missing")
            && path.is_some_and(|path| {
                std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn mark_candidate_needs_review(parsed: &mut ParsedCandidate) {
    if parsed.status != "ParseFailed" && parsed.status != "Unsupported" {
        parsed.status = "NeedsReview".to_owned();
    }
}

fn mark_multiple_pmx_issue(parsed: &mut ParsedCandidate) {
    mark_candidate_needs_review(parsed);
    if let Value::Object(metadata) = &mut parsed.metadata {
        metadata.insert(
            "candidate_reason".to_owned(),
            Value::String("multiple_primary_models_in_directory".to_owned()),
        );
    }
}

fn candidate_statuses(parsed: &ParsedCandidate) -> Vec<String> {
    let mut statuses = vec![parsed.status.clone()];
    let has_review_issue = parsed.metadata.get("candidate_reason").is_some()
        || parsed
            .metadata
            .get("card_identity_ambiguous")
            .and_then(Value::as_bool)
            == Some(true);
    if has_review_issue && !statuses.iter().any(|status| status == "NeedsReview") {
        statuses.push("NeedsReview".to_owned());
    }
    statuses
}

fn resolve_dependencies(path: &Path, parsed: &mut ParsedCandidate) {
    let source_dir = path.parent().unwrap_or(Path::new("."));
    let package_dir =
        std::fs::canonicalize(source_dir).unwrap_or_else(|_| normalized_absolute(source_dir));
    for dependency in &mut parsed.dependencies {
        let reference_path = PathBuf::from(&dependency.reference);
        let candidate = if reference_path.is_absolute() {
            reference_path
        } else {
            source_dir.join(reference_path)
        };
        let resolved =
            std::fs::canonicalize(&candidate).unwrap_or_else(|_| normalized_absolute(&candidate));
        let within_package = resolved.starts_with(&package_dir);
        let exists = std::fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file());
        dependency.path = Some(resolved.to_string_lossy().into_owned());
        dependency.status = if within_package && exists {
            "resolved".to_owned()
        } else if within_package {
            "missing".to_owned()
        } else {
            "external".to_owned()
        };
    }
    if let Value::Object(metadata) = &mut parsed.metadata {
        metadata.insert(
            "file_dependencies".to_owned(),
            serde_json::to_value(&parsed.dependencies).unwrap_or(Value::Array(Vec::new())),
        );
    }
    if parsed.dependencies.iter().any(|dependency| {
        dependency.role != "shared_toon_texture"
            && (dependency.status == "missing" || dependency.status == "external")
    }) && parsed.status == "Ready"
    {
        parsed.status = "NeedsReview".to_owned();
    }
}

fn normalized_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    normalize_path(&absolute)
}

fn scan_path_key(path: &Path) -> String {
    normalized_absolute(path)
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn metadata_modified_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0_i64, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn find_moved_asset(
    library: &Library,
    root: &Root,
    fingerprint: &str,
    current_paths: &HashSet<String>,
) -> CoreResult<Option<String>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT id,primary_source FROM assets WHERE root_id=?1 AND asset_type=?2 AND fingerprint=?3",
    )?;
    let rows = statement.query_map(
        params![root.id, root.asset_type.as_str(), fingerprint],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let missing_matches = rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(_, path)| !current_paths.contains(path))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    Ok((missing_matches.len() == 1).then(|| missing_matches[0].clone()))
}

fn parse_statuses(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_else(|_| vec!["NeedsReview".to_owned()])
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn is_binary_x_file(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut header = [0; 11];
    file.read_exact(&mut header)?;
    Ok(crate::x_binary::is_binary_x(&header))
}

fn multiple_pmx_directories(files: &[PathBuf], asset_type: AssetType) -> HashSet<PathBuf> {
    if asset_type == AssetType::Motion {
        return HashSet::new();
    }
    let mut counts = std::collections::HashMap::<PathBuf, usize>::new();
    for path in files.iter().filter(|path| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pmx"))
    }) {
        *counts
            .entry(path.parent().unwrap_or(Path::new(".")).to_path_buf())
            .or_default() += 1;
    }
    counts
        .into_iter()
        .filter_map(|(path, count)| (count > 1).then_some(path))
        .collect()
}

#[cfg(test)]
mod status_tests {
    use super::*;

    fn candidate_with_texture(name: &str, texture: &Path) -> ParsedCandidate {
        let dependency = crate::types::ParsedDependency {
            reference: texture
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            role: "texture".to_owned(),
            path: Some(texture.to_string_lossy().into_owned()),
            status: "resolved".to_owned(),
        };
        ParsedCandidate {
            name: name.to_owned(),
            metadata: serde_json::json!({
                "file_dependencies": [dependency.clone()]
            }),
            status: "Ready".to_owned(),
            dependencies: vec![dependency],
        }
    }

    #[test]
    fn candidate_review_issue_is_persisted_without_overwriting_status() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-candidate-status-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let primary = directory.join("broken.pmx");
        std::fs::write(&primary, b"fixture-pmx").unwrap();
        let library = Library::open(directory.join("library.sqlite3")).unwrap();
        let root = library
            .add_root(
                AssetType::Model,
                directory.to_str().unwrap(),
                Some("Candidate Status Fixture"),
            )
            .unwrap();
        let primary_metadata = std::fs::metadata(&primary).unwrap();
        let mut asset_id: Option<String> = None;

        for status in ["ParseFailed", "Unsupported"] {
            let mut parsed = ParsedCandidate {
                name: "Broken model".to_owned(),
                metadata: serde_json::json!({"error":{"error_code":status}}),
                status: status.to_owned(),
                dependencies: Vec::new(),
            };
            mark_multiple_pmx_issue(&mut parsed);

            assert_eq!(parsed.status, status);
            assert_eq!(
                candidate_statuses(&parsed),
                vec![status.to_owned(), "NeedsReview".to_owned()]
            );
            assert!(store_candidate(
                &library,
                &root,
                &primary,
                i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&primary_metadata),
                format!("{status}-fingerprint"),
                parsed,
                asset_id.as_deref(),
                None,
                "2026-09-27T00:00:00Z",
            )
            .unwrap());

            let connection = library.connection().unwrap();
            let (stored_id, statuses_json): (String, String) = connection
                .query_row(
                    "SELECT id,statuses_json FROM assets WHERE primary_source=?1",
                    [primary.to_string_lossy().as_ref()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            asset_id = Some(stored_id);
            assert_eq!(
                serde_json::from_str::<Vec<String>>(&statuses_json).unwrap(),
                vec![status.to_owned(), "NeedsReview".to_owned()]
            );
            let metadata_json: String = connection
                .query_row(
                    "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
                    [asset_id.as_deref().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            let metadata: Value = serde_json::from_str(&metadata_json).unwrap();
            assert_eq!(
                metadata.get("candidate_reason").and_then(Value::as_str),
                Some("multiple_primary_models_in_directory")
            );
        }

        drop(library);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn store_candidate_rolls_back_all_asset_rows_when_card_write_fails() {
        let directory = std::env::temp_dir().join(format!("mmdbridge-scan-transaction-{}", Uuid::new_v4()));
        let package = directory.join("model-package");
        let library_path = directory.join("library.sqlite3");
        std::fs::create_dir_all(&package).unwrap();
        let primary = package.join("model.pmx");
        let old_texture = package.join("old.png");
        let new_texture = package.join("new.png");
        std::fs::write(&primary, b"pmx").unwrap();
        std::fs::write(&old_texture, b"old").unwrap();
        std::fs::write(&new_texture, b"new").unwrap();

        let library = Library::open(&library_path).unwrap();
        let root = library
            .add_root(
                AssetType::Model,
                directory.to_str().unwrap(),
                Some("Transaction Fixture"),
            )
            .unwrap();
        let primary_metadata = std::fs::metadata(&primary).unwrap();
        store_candidate(
            &library,
            &root,
            &primary,
            i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
            metadata_modified_ns(&primary_metadata),
            "old-fingerprint".to_owned(),
            candidate_with_texture("Before", &old_texture),
            None,
            None,
            "2026-09-27T00:00:00Z",
        )
        .unwrap();
        let primary_text = primary.to_string_lossy().into_owned();
        let asset_id: String = library
            .connection()
            .unwrap()
            .query_row(
                "SELECT id FROM assets WHERE primary_source=?1",
                [&primary_text],
                |row| row.get(0),
            )
            .unwrap();
        let initial_card: (String, String, String) = library
            .connection()
            .unwrap()
            .query_row(
                "SELECT card_path,status,last_checked_at FROM cards WHERE asset_id=?1",
                [&asset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        let connection = library.connection().unwrap();
        connection
            .execute(
                "DELETE FROM asset_files WHERE asset_id=?1 AND role<>'primary'",
                [&asset_id],
            )
            .unwrap();
        drop(connection);
        assert!(!dependencies_unchanged(&library, &asset_id).unwrap());
        std::fs::remove_file(&old_texture).unwrap();
        std::fs::create_dir(&old_texture).unwrap();
        assert!(
            !store_candidate(
                &library,
                &root,
                &primary,
                i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&primary_metadata),
                "directory-dependency".to_owned(),
                candidate_with_texture("Directory", &old_texture),
                Some(&asset_id),
                None,
                "2026-09-27T00:00:01Z",
            )
            .unwrap()
        );
        let directory_metadata = std::fs::metadata(&old_texture).unwrap();
        library
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO asset_files(asset_id,path,role,file_size,modified_ns) VALUES (?1,?2,'texture',?3,?4)",
                params![
                    asset_id,
                    old_texture.to_string_lossy(),
                    i64::try_from(directory_metadata.len()).unwrap_or(i64::MAX),
                    metadata_modified_ns(&directory_metadata),
                ],
            )
            .unwrap();
        assert!(!dependencies_unchanged(&library, &asset_id).unwrap());
        std::fs::remove_dir(&old_texture).unwrap();
        std::fs::write(&old_texture, b"old").unwrap();
        let old_texture_metadata = std::fs::metadata(&old_texture).unwrap();
        library
            .connection()
            .unwrap()
            .execute(
                "UPDATE asset_files SET file_size=?2,modified_ns=?3 WHERE asset_id=?1 AND path=?4",
                params![
                    asset_id,
                    i64::try_from(old_texture_metadata.len()).unwrap_or(i64::MAX),
                    metadata_modified_ns(&old_texture_metadata),
                    old_texture.to_string_lossy(),
                ],
            )
            .unwrap();

        std::fs::remove_file(&new_texture).unwrap();
        assert!(
            !store_candidate(
                &library,
                &root,
                &primary,
                i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&primary_metadata),
                "new-fingerprint".to_owned(),
                candidate_with_texture("After", &new_texture),
                Some(&asset_id),
                None,
                "2026-09-27T00:00:01Z",
            )
            .unwrap()
        );
        std::fs::write(&new_texture, b"new").unwrap();
        library
            .connection()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_card_update BEFORE UPDATE ON cards
                 BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;",
            )
            .unwrap();

        let result = store_candidate(
            &library,
            &root,
            &primary,
            i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
            metadata_modified_ns(&primary_metadata),
            "new-fingerprint".to_owned(),
            candidate_with_texture("After", &new_texture),
            Some(&asset_id),
            None,
            "2026-09-27T00:00:01Z",
        );
        assert!(result.is_err());

        let connection = library.connection().unwrap();
        let (name, fingerprint, statuses): (String, String, String) = connection
            .query_row(
                "SELECT name,fingerprint,statuses_json FROM assets WHERE id=?1",
                [&asset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let dependencies: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT path FROM asset_files WHERE asset_id=?1 AND role<>'primary' ORDER BY path")
                .unwrap();
            statement
                .query_map([&asset_id], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let stored_metadata: String = connection
            .query_row(
                "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
                [&asset_id],
                |row| row.get(0),
            )
            .unwrap();
        let primary_file: (String, i64, i64) = connection
            .query_row(
                "SELECT path,file_size,modified_ns FROM asset_files WHERE asset_id=?1 AND role='primary'",
                [&asset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let stored_card: (String, String, String) = connection
            .query_row(
                "SELECT card_path,status,last_checked_at FROM cards WHERE asset_id=?1",
                [&asset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(name, "Before");
        assert_eq!(fingerprint, "old-fingerprint");
        assert_eq!(statuses, r#"["Ready"]"#);
        assert_eq!(dependencies, vec![old_texture.to_string_lossy().into_owned()]);
        assert_eq!(
            serde_json::from_str::<Value>(&stored_metadata).unwrap(),
            candidate_with_texture("Before", &old_texture).metadata
        );
        assert_eq!(
            primary_file,
            (
                primary_text,
                i64::try_from(primary_metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&primary_metadata)
            )
        );
        assert_eq!(stored_card, initial_card);

        drop(connection);
        drop(library);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
