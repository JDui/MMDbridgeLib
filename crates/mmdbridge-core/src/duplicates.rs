use std::collections::{HashMap, HashSet};

use rusqlite::params;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    CoreResult, Library,
    relations::{asset_name_key, asset_name_similarity},
    scanner::MAX_LIST_ITEMS,
    types::{AssetDuplicate, DuplicateRefreshReport},
};

const NAME_NEIGHBOR_WINDOW: usize = 24;
const GROUP_PAIR_LIMIT: usize = 40;
const GROUP_NEIGHBOR_WINDOW: usize = 8;
const POSSIBLE_DUPLICATE_THRESHOLD: f64 = 0.62;

#[derive(Clone)]
struct DuplicateCandidate {
    id: String,
    asset_type: String,
    name: String,
    path: String,
    fingerprint: String,
}

#[derive(Clone)]
struct SimilarityCandidate {
    id: String,
    asset_type: String,
    name: String,
    name_key: String,
    path: String,
    asset_directory: String,
    root_path: String,
    fingerprint: String,
    file_size: Option<i64>,
    polygon_count: Option<i64>,
    bone_count: Option<i64>,
    duration_seconds: Option<f64>,
    frame_count: Option<i64>,
}

struct PossibleDuplicate {
    asset_a: String,
    asset_b: String,
    similarity: f64,
    reason: Value,
}

pub(crate) fn rebuild(library: &Library) -> CoreResult<DuplicateRefreshReport> {
    let groups = load_exact_groups(library)?;
    let similarity_candidates = load_similarity_candidates(library)?;
    let possible_duplicates = find_possible_duplicates(&similarity_candidates);
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM duplicates", [])?;
    let mut pair_count = 0;
    let mut exact_duplicate_groups = 0;
    for (fingerprint, mut candidates) in groups
        .iter()
        .map(|(fingerprint, records)| (fingerprint.clone(), records.clone()))
    {
        candidates.sort_by(|left, right| {
            normalized_path(&left.path)
                .cmp(&normalized_path(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut distinct_sources = Vec::new();
        for candidate in candidates {
            let key = normalized_path(&candidate.path);
            if distinct_sources
                .last()
                .is_some_and(|previous: &DuplicateCandidate| normalized_path(&previous.path) == key)
            {
                continue;
            }
            distinct_sources.push(candidate);
        }
        let Some(canonical) = distinct_sources.first() else {
            continue;
        };
        if distinct_sources.len() < 2 {
            continue;
        }
        exact_duplicate_groups += 1;
        for duplicate in distinct_sources.iter().skip(1) {
            let reason = json!({
                "reason_codes": ["exact_content_hash"],
                "fingerprint": fingerprint,
                "asset_a_type": canonical.asset_type,
                "asset_b_type": duplicate.asset_type,
                "asset_a_name": canonical.name,
                "asset_b_name": duplicate.name,
                "source_a": canonical.path,
                "source_b": duplicate.path,
            });
            transaction.execute(
                "INSERT INTO duplicates(id,asset_a,asset_b,similarity,reason_json)
                 VALUES (?1,?2,?3,1.0,?4)",
                params![
                    Uuid::new_v4().to_string(),
                    canonical.id,
                    duplicate.id,
                    serde_json::to_string(&reason)?
                ],
            )?;
            pair_count += 1;
        }
    }
    let exact_duplicate_pairs = pair_count;
    let mut possible_duplicate_pairs = 0;
    for duplicate in possible_duplicates {
        transaction.execute(
            "INSERT INTO duplicates(id,asset_a,asset_b,similarity,reason_json)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                Uuid::new_v4().to_string(),
                duplicate.asset_a,
                duplicate.asset_b,
                duplicate.similarity,
                serde_json::to_string(&duplicate.reason)?
            ],
        )?;
        possible_duplicate_pairs += 1;
    }
    transaction.commit()?;

    Ok(DuplicateRefreshReport {
        exact_duplicate_groups,
        duplicate_pairs: exact_duplicate_pairs,
        possible_duplicate_pairs,
    })
}

fn load_similarity_candidates(library: &Library) -> CoreResult<Vec<SimilarityCandidate>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.root_id,a.asset_type,a.name,a.primary_source,a.asset_directory,a.fingerprint,
                r.path,f.file_size,m.value_json
         FROM assets a JOIN roots r ON r.id=a.root_id
         LEFT JOIN asset_files f ON f.asset_id=a.id AND f.role='primary'
         LEFT JOIN metadata m ON m.asset_id=a.id AND m.key='parsed'
         WHERE a.fingerprint LIKE 'blake3:%' AND instr(a.statuses_json,'MissingSource')=0
         ORDER BY a.asset_type,a.name COLLATE NOCASE,a.id",
    )?;
    let rows = statement.query_map([], |row| {
        let metadata_json: Option<String> = row.get(9)?;
        let metadata = metadata_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or(Value::Null);
        let name: String = row.get(3)?;
        Ok(SimilarityCandidate {
            id: row.get(0)?,
            asset_type: row.get(2)?,
            name_key: asset_name_key(&name),
            name,
            path: row.get(4)?,
            asset_directory: row.get(5)?,
            fingerprint: row.get(6)?,
            root_path: row.get(7)?,
            file_size: row.get(8)?,
            polygon_count: integer_metric(&metadata, "polygon_count"),
            bone_count: integer_metric(&metadata, "bone_count"),
            duration_seconds: metadata
                .get("duration_seconds")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value >= 0.0),
            frame_count: integer_metric(&metadata, "total_frames"),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn integer_metric(metadata: &Value, key: &str) -> Option<i64> {
    metadata.get(key)?.as_i64().or_else(|| {
        metadata
            .get(key)?
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
    })
}

fn find_possible_duplicates(candidates: &[SimilarityCandidate]) -> Vec<PossibleDuplicate> {
    let mut candidate_pairs = HashSet::<(usize, usize)>::new();
    let mut by_type = HashMap::<&str, Vec<usize>>::new();
    let mut by_directory = HashMap::<String, Vec<usize>>::new();
    let mut by_structure =
        HashMap::<(String, Option<i64>, Option<i64>, Option<i64>), Vec<usize>>::new();

    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.name_key.is_empty() {
            continue;
        }
        by_type
            .entry(candidate.asset_type.as_str())
            .or_default()
            .push(index);
        by_directory
            .entry(normalized_path(&candidate.asset_directory))
            .or_default()
            .push(index);

        let structure = (
            candidate.asset_type.clone(),
            candidate.polygon_count,
            candidate.bone_count,
            candidate.frame_count,
        );
        if structure.1.is_some() || structure.2.is_some() || structure.3.is_some() {
            by_structure.entry(structure).or_default().push(index);
        }
    }

    for indices in by_type.values_mut() {
        indices.sort_by(|left, right| {
            candidates[*left]
                .name_key
                .cmp(&candidates[*right].name_key)
                .then_with(|| candidates[*left].id.cmp(&candidates[*right].id))
        });
        add_neighbor_pairs(indices, NAME_NEIGHBOR_WINDOW, &mut candidate_pairs);
    }
    for indices in by_directory.values_mut() {
        sort_by_name(indices, candidates);
        add_group_pairs(
            indices,
            GROUP_PAIR_LIMIT,
            GROUP_NEIGHBOR_WINDOW,
            &mut candidate_pairs,
        );
    }
    for indices in by_structure.values_mut() {
        indices.sort_by(|left, right| {
            candidates[*left]
                .file_size
                .cmp(&candidates[*right].file_size)
                .then_with(|| candidates[*left].name_key.cmp(&candidates[*right].name_key))
                .then_with(|| candidates[*left].id.cmp(&candidates[*right].id))
        });
        add_group_pairs(
            indices,
            GROUP_PAIR_LIMIT,
            GROUP_NEIGHBOR_WINDOW,
            &mut candidate_pairs,
        );
    }

    let mut possible_duplicates = candidate_pairs
        .into_iter()
        .filter_map(|(left, right)| score_possible_duplicate(&candidates[left], &candidates[right]))
        .collect::<Vec<_>>();
    possible_duplicates.sort_by(|left, right| {
        right
            .similarity
            .total_cmp(&left.similarity)
            .then_with(|| left.asset_a.cmp(&right.asset_a))
            .then_with(|| left.asset_b.cmp(&right.asset_b))
    });
    possible_duplicates
}

fn sort_by_name(indices: &mut [usize], candidates: &[SimilarityCandidate]) {
    indices.sort_by(|left, right| {
        candidates[*left]
            .name_key
            .cmp(&candidates[*right].name_key)
            .then_with(|| candidates[*left].id.cmp(&candidates[*right].id))
    });
}

fn add_neighbor_pairs(indices: &[usize], window: usize, output: &mut HashSet<(usize, usize)>) {
    for (position, left) in indices.iter().copied().enumerate() {
        for right in indices.iter().copied().skip(position + 1).take(window) {
            output.insert((left.min(right), left.max(right)));
        }
    }
}

fn add_group_pairs(
    indices: &[usize],
    complete_limit: usize,
    neighbor_window: usize,
    output: &mut HashSet<(usize, usize)>,
) {
    if indices.len() <= complete_limit {
        for (position, left) in indices.iter().copied().enumerate() {
            for right in indices.iter().copied().skip(position + 1) {
                output.insert((left.min(right), left.max(right)));
            }
        }
    } else {
        add_neighbor_pairs(indices, neighbor_window, output);
    }
}

fn score_possible_duplicate(
    left: &SimilarityCandidate,
    right: &SimilarityCandidate,
) -> Option<PossibleDuplicate> {
    if left.asset_type != right.asset_type
        || left.fingerprint == right.fingerprint
        || normalized_path(&left.path) == normalized_path(&right.path)
    {
        return None;
    }

    let name_similarity = asset_name_similarity(&left.name, &right.name);
    let file_size_similarity = relative_similarity(left.file_size?, right.file_size?);
    let polygon_count_similarity =
        optional_integer_similarity(left.polygon_count, right.polygon_count);
    let bone_count_similarity = optional_integer_similarity(left.bone_count, right.bone_count);
    let duration_similarity =
        optional_number_similarity(left.duration_seconds, right.duration_seconds);
    let frame_count_similarity = optional_integer_similarity(left.frame_count, right.frame_count);
    let directory_similarity = directory_similarity(left, right);

    let similarity = (0.32 * name_similarity
        + 0.18 * file_size_similarity
        + 0.18 * polygon_count_similarity.unwrap_or(0.0)
        + 0.12 * bone_count_similarity.unwrap_or(0.0)
        + 0.08 * duration_similarity.unwrap_or(0.0)
        + 0.08 * frame_count_similarity.unwrap_or(0.0)
        + 0.04 * directory_similarity)
        .clamp(0.0, 0.999);
    if similarity < POSSIBLE_DUPLICATE_THRESHOLD {
        return None;
    }

    let (asset_a, asset_b) = if left.id <= right.id {
        (left, right)
    } else {
        (right, left)
    };
    let mut reason_codes = vec!["possible_duplicate"];
    if name_similarity >= 0.85 {
        reason_codes.push("high_name_similarity");
    }
    if polygon_count_similarity.is_some_and(|value| value >= 0.95)
        && bone_count_similarity.is_some_and(|value| value >= 0.95)
    {
        reason_codes.push("matching_model_structure");
    }
    if duration_similarity.is_some_and(|value| value >= 0.95)
        && frame_count_similarity.is_some_and(|value| value >= 0.95)
    {
        reason_codes.push("matching_motion_length");
    }
    if file_size_similarity >= 0.98 {
        reason_codes.push("similar_file_size");
    }
    if directory_similarity >= 0.95 {
        reason_codes.push("same_package_directory");
    }

    Some(PossibleDuplicate {
        asset_a: asset_a.id.clone(),
        asset_b: asset_b.id.clone(),
        similarity,
        reason: json!({
            "reason_codes": reason_codes,
            "asset_a_type": asset_a.asset_type,
            "asset_b_type": asset_b.asset_type,
            "asset_a_name": asset_a.name,
            "asset_b_name": asset_b.name,
            "source_a": asset_a.path,
            "source_b": asset_b.path,
            "fingerprint_a": asset_a.fingerprint,
            "fingerprint_b": asset_b.fingerprint,
            "components": {
                "name_similarity": name_similarity,
                "file_size_similarity": file_size_similarity,
                "polygon_count_similarity": polygon_count_similarity,
                "bone_count_similarity": bone_count_similarity,
                "duration_similarity": duration_similarity,
                "frame_count_similarity": frame_count_similarity,
                "directory_structure_similarity": directory_similarity,
                "content_fingerprint_match": false,
            },
            "threshold": POSSIBLE_DUPLICATE_THRESHOLD,
            "similarity": similarity,
        }),
    })
}

fn optional_integer_similarity(left: Option<i64>, right: Option<i64>) -> Option<f64> {
    optional_number_similarity(
        left.map(|value| value as f64),
        right.map(|value| value as f64),
    )
}

fn optional_number_similarity(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    let (left, right) = (left?, right?);
    if !left.is_finite() || !right.is_finite() || left < 0.0 || right < 0.0 {
        return None;
    }
    let larger = left.max(right);
    if larger == 0.0 {
        Some(1.0)
    } else {
        Some((left.min(right) / larger).clamp(0.0, 1.0))
    }
}

fn relative_similarity(left: i64, right: i64) -> f64 {
    optional_number_similarity(Some(left.max(0) as f64), Some(right.max(0) as f64)).unwrap_or(0.0)
}

fn directory_similarity(left: &SimilarityCandidate, right: &SimilarityCandidate) -> f64 {
    let left_components = relative_directory_components(&left.asset_directory, &left.root_path);
    let right_components = relative_directory_components(&right.asset_directory, &right.root_path);
    if left_components.is_empty() || right_components.is_empty() {
        return 0.0;
    }
    let common = left_components
        .iter()
        .zip(&right_components)
        .take_while(|(left, right)| left == right)
        .count();
    common as f64 / left_components.len().max(right_components.len()) as f64
}

fn relative_directory_components(directory: &str, root: &str) -> Vec<String> {
    let directory = path_components(directory);
    let root = path_components(root);
    let relative = if directory.len() >= root.len()
        && directory
            .iter()
            .zip(&root)
            .all(|(left, right)| left == right)
    {
        &directory[root.len()..]
    } else {
        &directory[..]
    };
    relative.to_vec()
}

fn path_components(path: &str) -> Vec<String> {
    path.replace('/', "\\")
        .split('\\')
        .filter(|component| !component.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(crate) fn list(
    library: &Library,
    asset_id: Option<&str>,
    limit: usize,
) -> CoreResult<Vec<AssetDuplicate>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT d.id,d.asset_a,a.name,a.primary_source,d.asset_b,b.name,b.primary_source,
                d.similarity,d.reason_json
         FROM duplicates d
         JOIN assets a ON a.id=d.asset_a
         JOIN assets b ON b.id=d.asset_b
         WHERE (?1 IS NULL OR d.asset_a=?1 OR d.asset_b=?1)
         ORDER BY d.similarity DESC,a.name COLLATE NOCASE,b.name COLLATE NOCASE
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![
            asset_id,
            i64::try_from(limit.min(MAX_LIST_ITEMS)).unwrap_or(MAX_LIST_ITEMS as i64)
        ],
        |row| {
            let reason_json: String = row.get(8)?;
            Ok(AssetDuplicate {
                id: row.get(0)?,
                asset_a: row.get(1)?,
                asset_a_name: row.get(2)?,
                asset_a_path: row.get(3)?,
                asset_b: row.get(4)?,
                asset_b_name: row.get(5)?,
                asset_b_path: row.get(6)?,
                similarity: row.get(7)?,
                reason: serde_json::from_str(&reason_json).unwrap_or(Value::Null),
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn count(library: &Library) -> CoreResult<usize> {
    let connection = library.connection()?;
    let count = connection.query_row("SELECT COUNT(*) FROM duplicates", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn load_exact_groups(library: &Library) -> CoreResult<HashMap<String, Vec<DuplicateCandidate>>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT id,asset_type,name,primary_source,fingerprint FROM assets
         WHERE fingerprint LIKE 'blake3:%' AND instr(statuses_json,'MissingSource')=0
         ORDER BY fingerprint,primary_source COLLATE NOCASE,id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(DuplicateCandidate {
            id: row.get(0)?,
            asset_type: row.get(1)?,
            name: row.get(2)?,
            path: row.get(3)?,
            fingerprint: row.get(4)?,
        })
    })?;
    let mut groups = HashMap::<String, Vec<DuplicateCandidate>>::new();
    for candidate in rows {
        let candidate = candidate?;
        groups
            .entry(candidate.fingerprint.clone())
            .or_default()
            .push(candidate);
    }
    groups.retain(|_, candidates| candidates.len() > 1);
    Ok(groups)
}

fn normalized_path(path: &str) -> String {
    path.replace('/', "\\")
        .to_lowercase()
        .trim_end_matches('\\')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_model(
        library: &Library,
        root_id: &str,
        root_path: &str,
        id: &str,
        name: &str,
        file_name: &str,
        fingerprint: &str,
        file_size: i64,
        polygon_count: i64,
        bone_count: i64,
    ) -> CoreResult<()> {
        let asset_directory = format!("{root_path}\\Characters");
        let source = format!("{asset_directory}\\{file_name}.pmx");
        let now = "2026-09-25T00:00:00Z";
        let metadata = json!({"polygon_count": polygon_count, "bone_count": bone_count});
        let connection = library.connection()?;
        connection.execute(
            "INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,statuses_json,created_at,updated_at,last_seen_at)
             VALUES (?1,?2,'model',?3,?4,?5,?6,'[]',?7,?7,?7)",
            rusqlite::params![id, root_id, name, source, asset_directory, fingerprint, now],
        )?;
        connection.execute(
            "INSERT INTO asset_files(asset_id,path,role,file_size,modified_ns) VALUES (?1,?2,'primary',?3,0)",
            rusqlite::params![id, source, file_size],
        )?;
        connection.execute(
            "INSERT INTO metadata(asset_id,key,value_json) VALUES (?1,'parsed',?2)",
            rusqlite::params![id, serde_json::to_string(&metadata)?],
        )?;
        Ok(())
    }

    #[test]
    fn rebuild_persists_possible_matches_without_merging_exact_or_unrelated_assets()
    -> CoreResult<()> {
        let library = Library::in_memory()?;
        let root_path = std::env::temp_dir().to_string_lossy().into_owned();
        let connection = library.connection()?;
        connection.execute(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at)
             VALUES ('test-root','model',?1,?1,'Test Root','2026-09-25T00:00:00Z')",
            [&root_path],
        )?;
        drop(connection);

        insert_model(
            &library,
            "test-root",
            &root_path,
            "a",
            "Miku v1",
            "miku-v1",
            "blake3:exact-content",
            10_000_000,
            30_000,
            120,
        )?;
        insert_model(
            &library,
            "test-root",
            &root_path,
            "b",
            "Miku v2",
            "miku-v2",
            "blake3:edited-content",
            10_100_000,
            30_100,
            120,
        )?;
        insert_model(
            &library,
            "test-root",
            &root_path,
            "c",
            "Miku v3",
            "miku-v3",
            "blake3:exact-content",
            10_000_000,
            30_000,
            120,
        )?;
        insert_model(
            &library,
            "test-root",
            &root_path,
            "d",
            "Tree Stage",
            "tree-stage",
            "blake3:unrelated-content",
            5_000_000,
            2_000,
            8,
        )?;

        let report = library.rebuild_duplicates()?;
        let matches = library.list_duplicates(None, 100)?;
        assert_eq!(report.exact_duplicate_groups, 1);
        assert_eq!(report.duplicate_pairs, 1);
        assert!(report.possible_duplicate_pairs >= 1);
        assert!(matches.iter().any(|item| {
            item.asset_a == "a"
                && item.asset_b == "c"
                && item.reason["reason_codes"]
                    .as_array()
                    .is_some_and(|codes| codes.iter().any(|code| code == "exact_content_hash"))
        }));
        assert!(matches.iter().any(|item| {
            item.asset_a == "a"
                && item.asset_b == "b"
                && item.similarity < 1.0
                && item.reason["reason_codes"]
                    .as_array()
                    .is_some_and(|codes| codes.iter().any(|code| code == "possible_duplicate"))
        }));
        assert!(
            !matches
                .iter()
                .any(|item| item.asset_a == "a" && item.asset_b == "d")
        );
        Ok(())
    }
}
