use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use strsim::normalized_levenshtein;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    CoreError, CoreResult, Library,
    types::{AssetRelation, RelationRefreshReport},
};

const CAMERA_RELATION: &str = "MotionCameraPair";
const VERSION_RELATION: &str = "VersionFamily";
const MAX_RELATION_DIRECTORY_DEPTH: usize = 2;

#[derive(Clone)]
struct MotionAsset {
    id: String,
    root_id: String,
    source: String,
    directory: String,
    stem: String,
    has_camera: bool,
    has_motion: bool,
    camera_only: bool,
    parsed: bool,
}

struct Proposal {
    relation_type: &'static str,
    source_asset: String,
    target_asset: String,
    confidence: f64,
    reason: Value,
}

struct VersionInfo {
    core_name: String,
    label: String,
    tokens: Vec<String>,
}

struct VersionFamilyGroup {
    family_id: String,
    members: Vec<(MotionAsset, VersionInfo)>,
    core_name: String,
    shared_directory: String,
}

pub(crate) fn rebuild(library: &Library) -> CoreResult<RelationRefreshReport> {
    let assets = load_motion_assets(library)?;
    let mut proposals = camera_proposals(&assets);
    let motion_camera_pairs = proposals.len();
    let families = version_families(&assets);
    for family in &families {
        for left in 0..family.members.len() {
            for right in left + 1..family.members.len() {
                let (source, source_version) = &family.members[left];
                let (target, target_version) = &family.members[right];
                let reason_codes =
                    if source_version.tokens.is_empty() || target_version.tokens.is_empty() {
                        vec!["shared_core_name", "version_baseline_pair"]
                    } else {
                        vec!["shared_core_name", "version_tokens_detected"]
                    };
                proposals.push(Proposal {
                    relation_type: VERSION_RELATION,
                    source_asset: source.id.clone(),
                    target_asset: target.id.clone(),
                    confidence: 0.9,
                    reason: json!({
                        "reason_codes": reason_codes,
                        "core_name": family.core_name,
                        "source_path": source.source,
                        "target_path": target.source,
                        "source_version": source_version.label,
                        "target_version": target_version.label,
                        "shared_directory": family.shared_directory,
                    }),
                });
            }
        }
    }

    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    let confirmed = {
        let mut statement = transaction.prepare(
            "SELECT relation_type,source_asset,target_asset FROM relations WHERE confirmed=1",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<HashSet<_>, _>>()?
    };
    transaction.execute(
        "DELETE FROM relations WHERE confirmed=0 AND relation_type IN (?1,?2)",
        params![CAMERA_RELATION, VERSION_RELATION],
    )?;
    transaction.execute("DELETE FROM versions", [])?;

    let mut relation_proposals = 0;
    for proposal in proposals.drain(..) {
        if confirmed.contains(&(
            proposal.relation_type.to_owned(),
            proposal.source_asset.clone(),
            proposal.target_asset.clone(),
        )) {
            continue;
        }
        transaction.execute(
            "INSERT INTO relations(id,relation_type,source_asset,target_asset,confidence,reason_json,confirmed)
             VALUES (?1,?2,?3,?4,?5,?6,0)",
            params![
                Uuid::new_v4().to_string(),
                proposal.relation_type,
                proposal.source_asset,
                proposal.target_asset,
                proposal.confidence,
                serde_json::to_string(&proposal.reason)?
            ],
        )?;
        relation_proposals += 1;
    }

    for family in &families {
        for (asset, version) in &family.members {
            let reason = json!({
                "reason_codes": ["shared_core_name", "version_tokens_detected"],
                "core_name": family.core_name,
                "path": asset.source,
                "version_tokens": version.tokens,
                "shared_directory": family.shared_directory,
            });
            transaction.execute(
                "INSERT INTO versions(family_id,asset_id,version_label,confidence,reason_json)
                 VALUES (?1,?2,?3,0.9,?4)",
                params![
                    family.family_id,
                    asset.id,
                    version.label,
                    serde_json::to_string(&reason)?
                ],
            )?;
        }
    }
    transaction.commit()?;

    Ok(RelationRefreshReport {
        motion_camera_pairs,
        version_families: families.len(),
        relation_proposals,
    })
}

pub(crate) fn list(
    library: &Library,
    asset_id: Option<&str>,
    relation_type: Option<&str>,
    limit: usize,
) -> CoreResult<Vec<AssetRelation>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT r.id,r.relation_type,r.source_asset,r.target_asset,r.confidence,r.reason_json,r.confirmed,
                source.primary_source,target.primary_source
         FROM relations r JOIN assets source ON source.id=r.source_asset
         JOIN assets target ON target.id=r.target_asset
         WHERE (?1 IS NULL OR r.source_asset=?1 OR r.target_asset=?1)
           AND (?2 IS NULL OR r.relation_type=?2)
         ORDER BY r.confirmed DESC,r.confidence DESC,r.relation_type,r.source_asset,r.target_asset
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            asset_id,
            relation_type,
            i64::try_from(limit.min(10_000)).unwrap_or(10_000)
        ],
        |row| {
            let reason_json: String = row.get(5)?;
            let mut reason = serde_json::from_str::<Value>(&reason_json)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            reason.insert("source_path".to_owned(), json!(row.get::<_, String>(7)?));
            reason.insert("target_path".to_owned(), json!(row.get::<_, String>(8)?));
            Ok(AssetRelation {
                id: row.get(0)?,
                relation_type: row.get(1)?,
                source_asset: row.get(2)?,
                target_asset: row.get(3)?,
                confidence: row.get(4)?,
                reason: Value::Object(reason),
                confirmed: row.get::<_, i64>(6)? != 0,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn confirm(library: &Library, relation_id: &str) -> CoreResult<bool> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    let relation: Option<(String, String, String, bool, bool, bool)> = transaction
        .query_row(
            "SELECT r.relation_type,r.source_asset,r.target_asset,
                    COALESCE(json_extract(target_metadata.value_json,'$.has_camera'),0)=1,
                    COALESCE(json_extract(source_metadata.value_json,'$.has_bone_motion'),0)=1
                      OR COALESCE(json_extract(source_metadata.value_json,'$.has_morph_motion'),0)=1,
                    target.retired_format=0 AND instr(target.statuses_json,'MissingSource')=0
             FROM relations r
             JOIN assets source ON source.id=r.source_asset
             JOIN assets target ON target.id=r.target_asset
             LEFT JOIN metadata source_metadata ON source_metadata.asset_id=source.id AND source_metadata.key='parsed'
             LEFT JOIN metadata target_metadata ON target_metadata.asset_id=target.id AND target_metadata.key='parsed'
             WHERE r.id=?1",
            [relation_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((relation_type, source_id, _target_id, target_has_camera, source_has_motion, target_available)) = relation else {
        return Ok(false);
    };
    if relation_type == CAMERA_RELATION {
        if !target_has_camera || !source_has_motion || !target_available {
            return Err(CoreError::AssetOperation(
                "配套 Camera 关系已失效，请先重新扫描并刷新关系建议".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE relations SET confirmed=0
             WHERE relation_type=?1 AND source_asset=?2 AND id!=?3",
            params![CAMERA_RELATION, source_id, relation_id],
        )?;
    }
    let changed = transaction.execute(
        "UPDATE relations SET confirmed=1 WHERE id=?1",
        [relation_id],
    )? > 0;
    transaction.commit()?;
    Ok(changed)
}

fn load_motion_assets(library: &Library) -> CoreResult<Vec<MotionAsset>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.root_id,a.primary_source,a.asset_directory,m.value_json,a.visibility
         FROM assets a LEFT JOIN metadata m ON m.asset_id=a.id AND m.key='parsed'
         WHERE a.asset_type='motion' AND instr(a.statuses_json,'MissingSource')=0
         ORDER BY a.root_id,a.primary_source COLLATE NOCASE",
    )?;
    let rows = statement.query_map([], |row| {
        let source: String = row.get(2)?;
        let metadata_json: Option<String> = row.get(4)?;
        let visibility: String = row.get(5)?;
        let metadata = metadata_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        let has_camera = metadata
            .get("has_camera")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_motion = metadata
            .get("has_bone_motion")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || metadata
                .get("has_morph_motion")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let camera_only = visibility == "auxiliary";
        let parsed = metadata.is_object()
            && metadata.get("error").is_none()
            && metadata.get("file_type").is_some();
        let stem = std::path::Path::new(&source)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        Ok(MotionAsset {
            id: row.get(0)?,
            root_id: row.get(1)?,
            source,
            directory: row.get(3)?,
            stem,
            has_camera,
            has_motion,
            camera_only,
            parsed,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn camera_proposals(assets: &[MotionAsset]) -> Vec<Proposal> {
    let mut by_directory = HashMap::<(String, String), Vec<usize>>::new();
    let mut children = HashMap::<(String, String), Vec<String>>::new();
    for (index, asset) in assets.iter().enumerate() {
        let directory_key = path_key(&asset.directory);
        by_directory
            .entry((asset.root_id.clone(), directory_key.clone()))
            .or_default()
            .push(index);
        if let Some(parent) = std::path::Path::new(&asset.directory).parent() {
            let parent_key = path_key(&parent.to_string_lossy());
            children
                .entry((asset.root_id.clone(), parent_key))
                .or_default()
                .push(directory_key);
        }
    }
    for directories in children.values_mut() {
        directories.sort();
        directories.dedup();
    }

    let mut proposals = Vec::new();
    let mut emitted = HashSet::new();
    for motion in assets.iter().filter(|asset| is_motion_asset(asset)) {
        let base = path_key(&motion.directory);
        let mut directories = vec![(base.clone(), 0usize)];
        let first_level = children
            .get(&(motion.root_id.clone(), base.clone()))
            .cloned()
            .unwrap_or_default();
        for directory in first_level {
            directories.push((directory.clone(), 1));
            if let Some(second_level) = children.get(&(motion.root_id.clone(), directory)) {
                directories.extend(second_level.iter().cloned().map(|child| (child, 2)));
            }
        }

        for (directory, depth) in directories {
            let Some(candidates) = by_directory.get(&(motion.root_id.clone(), directory.clone()))
            else {
                continue;
            };
            for candidate_index in candidates {
                let camera = &assets[*candidate_index];
                if camera.id == motion.id || !is_camera_asset(camera) {
                    continue;
                }
                let key = (motion.id.clone(), camera.id.clone());
                if !emitted.insert(key) {
                    continue;
                }
                let scored = score_camera_pair(motion, camera, depth);
                if scored.confidence >= 0.52 {
                    proposals.push(scored);
                }
            }
        }
    }
    proposals
}

fn is_motion_asset(asset: &MotionAsset) -> bool {
    asset.has_motion && !is_camera_asset(asset)
}

fn is_camera_asset(asset: &MotionAsset) -> bool {
    asset.has_camera
        && (asset.camera_only
            || has_camera_channel(&asset.stem)
            || asset.directory.split(['\\', '/']).any(is_camera_channel))
}

fn score_camera_pair(
    motion: &MotionAsset,
    camera: &MotionAsset,
    directory_depth: usize,
) -> Proposal {
    let motion_terms = remove_channel_words(&motion.stem);
    let camera_terms = remove_channel_words(&camera.stem);
    let motion_name = compact_name(&motion_terms);
    let camera_name = compact_name(&camera_terms);
    let normalized_filename = if !motion_name.is_empty() && motion_name == camera_name {
        1.0
    } else {
        0.0
    };
    let prefix_similarity = common_prefix_ratio(&motion_name, &camera_name);
    let token_similarity = token_overlap(&motion_terms, &camera_terms);
    let edit_similarity = if motion_name.is_empty() || camera_name.is_empty() {
        0.0
    } else {
        normalized_levenshtein(&motion_name, &camera_name)
    };
    let has_camera_suffix = has_camera_channel(&camera.stem);
    let has_camera_folder = camera.directory.split(['\\', '/']).any(is_camera_channel);
    let camera_evidence = if has_camera_suffix || has_camera_folder {
        1.0
    } else {
        0.45
    };
    let directory_proximity = 1.0 - (directory_depth as f64 * 0.2);
    let version_motion = version_info(&motion.stem);
    let version_camera = version_info(&camera.stem);
    let version_conflict = !version_motion.tokens.is_empty()
        && !version_camera.tokens.is_empty()
        && version_motion.tokens != version_camera.tokens;
    let version_agreement = if version_conflict { 0.0 } else { 1.0 };
    let compatible_asset_type = 1.0; // Both records were read from the Motion root.
    let mut confidence = 0.20 * normalized_filename
        + 0.09 * prefix_similarity
        + 0.10 * token_similarity
        + 0.20 * edit_similarity
        + 0.15 * camera_evidence
        + 0.10 * directory_proximity
        + 0.10 * compatible_asset_type
        + 0.06 * version_agreement;
    if version_conflict {
        confidence -= 0.18;
    }
    confidence = confidence.clamp(0.0, 1.0);

    let mut reason_codes = vec!["camera_asset_detected"];
    if normalized_filename == 1.0 {
        reason_codes.push("normalized_filename_match");
    }
    if has_camera_suffix {
        reason_codes.push("camera_suffix_detected");
    }
    if has_camera_folder {
        reason_codes.push("camera_folder_detected");
    }
    reason_codes.push(match directory_depth {
        0 => "same_parent_directory",
        1 => "directory_distance_1",
        _ => "directory_distance_2",
    });
    if version_conflict {
        reason_codes.push("version_token_conflict");
    } else if !version_motion.tokens.is_empty() && version_motion.tokens == version_camera.tokens {
        reason_codes.push("version_tokens_match");
    }

    Proposal {
        relation_type: CAMERA_RELATION,
        source_asset: motion.id.clone(),
        target_asset: camera.id.clone(),
        confidence,
        reason: json!({
            "reason_codes": reason_codes,
            "components": {
                "normalized_filename": normalized_filename,
                "common_prefix": prefix_similarity,
                "common_tokens": token_similarity,
                "edit_distance_similarity": edit_similarity,
                "camera_evidence": camera_evidence,
                "directory_proximity": directory_proximity,
                "compatible_asset_type": compatible_asset_type,
                "version_agreement": version_agreement,
            },
            "core_name_match": !motion_name.is_empty() && motion_name == camera_name,
            "motion_path": motion.source,
            "camera_path": camera.source,
            "motion_version": version_motion.label,
            "camera_version": version_camera.label,
            "confidence": confidence,
        }),
    }
}

fn version_families(assets: &[MotionAsset]) -> Vec<VersionFamilyGroup> {
    let mut candidates = HashMap::<(String, bool, String), Vec<(MotionAsset, VersionInfo)>>::new();
    for asset in assets {
        if !asset.parsed {
            continue;
        }
        let version = version_info(&asset.stem);
        if version.core_name.is_empty() {
            continue;
        }
        candidates
            .entry((
                asset.root_id.clone(),
                is_camera_asset(asset),
                version.core_name.clone(),
            ))
            .or_default()
            .push((asset.clone(), version));
    }

    let mut families = Vec::new();
    for ((root_id, camera_only, core_name), members) in candidates {
        if members.len() < 2
            || !members
                .iter()
                .any(|(_, version)| !version.tokens.is_empty())
        {
            continue;
        }
        let mut parents = (0..members.len()).collect::<Vec<_>>();
        for left in 0..members.len() {
            for right in left + 1..members.len() {
                if common_directory(&members[left].0.directory, &members[right].0.directory)
                    .is_some()
                {
                    union(&mut parents, left, right);
                }
            }
        }
        let mut components = HashMap::<usize, Vec<(MotionAsset, VersionInfo)>>::new();
        for (index, member) in members.into_iter().enumerate() {
            let root = find(&mut parents, index);
            components.entry(root).or_default().push(member);
        }
        for mut component in components.into_values().filter(|members| members.len() > 1) {
            component
                .sort_by(|left, right| path_key(&left.0.source).cmp(&path_key(&right.0.source)));
            let shared_directory = component.iter().skip(1).fold(
                component[0].0.directory.clone(),
                |shared, (asset, _)| {
                    common_directory(&shared, &asset.directory)
                        .map(|value| value.0)
                        .unwrap_or_default()
                },
            );
            let family_id = format!(
                "vf:{root_id}:{}:{}:{}",
                if camera_only { "camera" } else { "motion" },
                core_name,
                path_key(&shared_directory)
            );
            families.push(VersionFamilyGroup {
                family_id,
                members: component,
                core_name: core_name.clone(),
                shared_directory,
            });
        }
    }
    families.sort_by(|left, right| left.family_id.cmp(&right.family_id));
    families
}

fn version_info(stem: &str) -> VersionInfo {
    let without_channels = remove_channel_words(stem);
    let words = without_channels
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut base = Vec::new();
    let mut versions = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let word = words[index].to_lowercase();
        if matches!(word.as_str(), "v" | "ver" | "version")
            && words
                .get(index + 1)
                .is_some_and(|next| next.chars().all(char::is_numeric))
        {
            versions.push(format!("{}{}", word, words[index + 1]));
            index += 2;
            continue;
        }
        if is_version_word(&word) || is_version_number(&word) && index + 1 == words.len() {
            versions.push(word);
        } else {
            base.push(word);
        }
        index += 1;
    }
    let core_name = base.concat();
    let label = if versions.is_empty() {
        "Original".to_owned()
    } else {
        versions.join("+")
    };
    VersionInfo {
        core_name,
        label,
        tokens: versions,
    }
}

fn is_version_word(word: &str) -> bool {
    let lowered = word.to_lowercase();
    [
        "fix",
        "fixed",
        "final",
        "revised",
        "rev",
        "修正",
        "修正版",
        "改",
        "改訂",
        "最新版",
    ]
    .iter()
    .any(|known| lowered == *known)
        || lowered
            .strip_prefix('v')
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(char::is_numeric))
        || lowered
            .strip_prefix("ver")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(char::is_numeric))
        || lowered
            .strip_prefix("version")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(char::is_numeric))
}

fn is_version_number(word: &str) -> bool {
    !word.is_empty() && word.chars().all(char::is_numeric)
}

fn compact_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn normalize_name(value: &str) -> String {
    value.nfc().flat_map(char::to_lowercase).collect()
}

fn remove_channel_words(value: &str) -> String {
    let normalized = normalize_name(value);
    normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| {
            !matches!(
                *word,
                "camera"
                    | "cam"
                    | "カメラ"
                    | "camera_motion"
                    | "motion"
                    | "モーション"
                    | "动作"
                    | "动作数据"
                    | "相机"
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_camera_channel(value: &str) -> bool {
    let normalized = normalize_name(value);
    normalized
        .split(|character: char| !character.is_alphanumeric())
        .any(|part| matches!(part, "camera" | "cam" | "カメラ" | "相机"))
}

fn is_camera_channel(value: &str) -> bool {
    has_camera_channel(value)
}

fn common_prefix_ratio(left: &str, right: &str) -> f64 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let common = left
        .iter()
        .zip(&right)
        .take_while(|(left, right)| left == right)
        .count();
    let longest = left.len().max(right.len());
    if longest == 0 {
        0.0
    } else {
        common as f64 / longest as f64
    }
}

fn token_overlap(left: &str, right: &str) -> f64 {
    let left = left
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<HashSet<_>>();
    let right = right
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<HashSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count();
    let union = left.union(&right).count();
    intersection as f64 / union as f64
}

fn path_key(path: &str) -> String {
    path.replace('/', "\\")
        .to_lowercase()
        .trim_end_matches('\\')
        .to_owned()
}

fn common_directory(left: &str, right: &str) -> Option<(String, usize, usize)> {
    let left_components = path_components(left);
    let right_components = path_components(right);
    let common = left_components
        .iter()
        .zip(&right_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }
    let left_distance = left_components.len().saturating_sub(common);
    let right_distance = right_components.len().saturating_sub(common);
    if left_distance > MAX_RELATION_DIRECTORY_DEPTH || right_distance > MAX_RELATION_DIRECTORY_DEPTH
    {
        return None;
    }
    let shared = left_components[..common].join("\\");
    Some((shared, left_distance, right_distance))
}

fn path_components(path: &str) -> Vec<String> {
    path.split(['\\', '/'])
        .filter(|component| !component.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn find(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        let root = find(parents, parents[index]);
        parents[index] = root;
    }
    parents[index]
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left_root = find(parents, left);
    let right_root = find(parents, right);
    if left_root != right_root {
        parents[right_root] = left_root;
    }
}
