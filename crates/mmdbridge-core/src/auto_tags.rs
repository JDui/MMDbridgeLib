use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AssetType, CoreError, CoreResult, Library};

const REVISION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AutoTagSettings {
    pub technical: bool,
    pub colors: bool,
}

impl Default for AutoTagSettings {
    fn default() -> Self { Self { technical: true, colors: true } }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubjectColor {
    pub name: String,
    pub share: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubjectPalette {
    pub algorithm_version: u32,
    pub status: String,
    pub sampled_pixels: usize,
    pub colors: Vec<SubjectColor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct OwnedTags {
    revision: u32,
    fingerprint: String,
    names: Vec<String>,
    #[serde(default)]
    requested: Vec<String>,
}

impl Library {
    pub fn auto_tag_settings(&self) -> CoreResult<AutoTagSettings> {
        let value: Option<String> = self.connection()?.query_row(
            "SELECT value_json FROM settings WHERE key='auto_tags'", [], |row| row.get(0),
        ).optional()?;
        value.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose().map(|value| value.unwrap_or_default())
    }

    pub fn set_auto_tag_settings(&self, settings: &AutoTagSettings) -> CoreResult<AutoTagSettings> {
        let previous = self.auto_tag_settings()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO settings(key,value_json,updated_at) VALUES ('auto_tags',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
            params![serde_json::to_string(settings)?, Utc::now().to_rfc3339()],
        )?;
        if settings.colors && !previous.colors {
            // A subsequent queued scan can analyse existing previews through the
            // cache even when their technical facts were already up to date.
            transaction.execute(
                "UPDATE cards SET status='CardStale' WHERE status='CardValid' AND asset_id IN
                 (SELECT id FROM assets WHERE asset_type='model' AND visibility='normal' AND retired_format=0)", [],
            )?;
        }
        transaction.commit()?;
        Ok(settings.clone())
    }
}

pub(crate) fn technical_tags(asset_type: AssetType, metadata: &Value) -> Vec<String> {
    if metadata.get("error").is_some() || metadata["is_camera_only"] == true { return Vec::new(); }
    let Some(format) = metadata["file_type"].as_str().filter(|format| matches!(*format, "pmx" | "pmd" | "vmd" | "vpd")) else { return Vec::new(); };
    let mut tags = vec![format!("格式:{}", format.to_ascii_uppercase()), format!("资产:{}", match asset_type {
        AssetType::Model => "模型", AssetType::Motion => "动作", AssetType::Scene => "场景",
    })];
    let count = |key: &str| metadata[key].as_u64().unwrap_or(0);
    if matches!(asset_type, AssetType::Model | AssetType::Scene) {
        for (key, name) in [("rigid_body_count", "技术:含物理"), ("usable_morph_count", "技术:含Morph"),
            ("sdef_vertex_count", "技术:含SDEF"), ("qdef_vertex_count", "技术:含QDEF"),
            ("vertex_morph_count", "技术:含顶点Morph"), ("bone_morph_count", "技术:含骨骼Morph"),
            ("material_morph_count", "技术:含材质Morph"), ("uv_morph_count", "技术:含UVMorph")] {
            if count(key) > 0 { tags.push(name.to_owned()); }
        }
        if asset_type == AssetType::Model {
            if let Some(name) = match metadata["skeleton_class"].as_str() {
                Some("standard") => Some("骨架:标准MMD"), Some("nonstandard") => Some("骨架:非标准"), _ => None,
            } { tags.push(name.to_owned()); }
        }
    } else {
        for (key, name) in [("has_bone_motion", "轨道:含骨骼"), ("has_morph_motion", "轨道:含表情"),
            ("has_camera", "轨道:含镜头"), ("has_light", "轨道:含灯光"), ("has_ik", "轨道:含IK"),
            ("has_self_shadow", "轨道:含自阴影")] {
            if metadata[key] == true { tags.push(name.to_owned()); }
        }
        if format == "vpd" && count("bone_count") > 0 { tags.push("轨道:含骨骼".to_owned()); }
        if metadata["is_pose"] == true { tags.push("动作:静态姿势".to_owned()); }
    }
    tags
}

// Only assignments recorded by this engine are replaced. Existing user/agent/legacy
// parser assignments and manual removal overrides remain authoritative.
pub(crate) fn reconcile(
    transaction: &Transaction<'_>, asset_id: &str, stage: &str, fingerprint: &str,
    candidates: &[String], confidence: f64,
) -> CoreResult<bool> {
    let key = format!("auto_tags_{stage}");
    let old: Option<String> = transaction.query_row(
        "SELECT value_json FROM metadata WHERE asset_id=?1 AND key=?2", params![asset_id, key], |row| row.get(0),
    ).optional()?;
    let previous = old.as_deref().map(serde_json::from_str::<OwnedTags>).transpose()?;
    let owned: HashSet<&str> = previous.as_ref().map(|state| state.names.iter().map(String::as_str).collect()).unwrap_or_default();
    let mut changed = false;
    for name in owned.iter().filter(|name| !candidates.iter().any(|candidate| candidate == **name)) {
        changed |= transaction.execute(
            "DELETE FROM asset_tags WHERE asset_id=?1 AND source='parser'
             AND tag_id IN (SELECT id FROM tags WHERE name=?2 COLLATE NOCASE)", params![asset_id, name],
        )? > 0;
    }
    let mut names = Vec::new();
    for name in candidates {
        let existing: Option<String> = transaction.query_row(
            "SELECT at.source FROM asset_tags at JOIN tags t ON at.tag_id=t.id
             WHERE at.asset_id=?1 AND t.name=?2 COLLATE NOCASE", params![asset_id, name], |row| row.get(0),
        ).optional()?;
        if existing.as_deref().is_some_and(|source| source != "parser" || !owned.contains(name.as_str())) { continue; }
        let mutation = Library::add_asset_tag_in_transaction(transaction, asset_id, name, "parser", Some(confidence))?;
        changed |= mutation.changed;
        if !mutation.blocked_by_user { names.push(name.clone()); }
    }
    let state = OwnedTags { revision: REVISION, fingerprint: fingerprint.to_owned(), names, requested: candidates.to_vec() };
    if previous.as_ref() != Some(&state) {
        transaction.execute(
            "INSERT INTO metadata(asset_id,key,value_json) VALUES (?1,?2,?3)
             ON CONFLICT(asset_id,key) DO UPDATE SET value_json=excluded.value_json",
            params![asset_id, key, serde_json::to_string(&state)?],
        )?;
    }
    if changed {
        transaction.execute("UPDATE cards SET status='CardStale' WHERE asset_id=?1 AND status='CardValid'", [asset_id])?;
    }
    Ok(changed)
}

pub(crate) fn backfill(library: &Library, asset_id: &str, asset_type: AssetType, metadata: &Value, fingerprint: &str) -> CoreResult<bool> {
    if !library.auto_tag_settings()?.technical || metadata["is_camera_only"] == true { return Ok(false); }
    let candidates = technical_tags(asset_type, metadata);
    let cached: Option<String> = library.connection()?.query_row(
        "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='auto_tags_technical'", [asset_id], |row| row.get(0),
    ).optional()?;
    let cached = cached.as_deref().map(serde_json::from_str::<OwnedTags>).transpose()?;
    if cached.as_ref().is_some_and(|state| state.revision == REVISION && state.fingerprint == fingerprint && state.requested == candidates) {
        // User additions/removals already update their assignments/overrides through
        // Core. An unchanged asset needs neither a write transaction nor per-tag SQL.
        return Ok(false);
    }
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    let changed = reconcile(&transaction, asset_id, "technical", fingerprint, &candidates, 1.0)?;
    transaction.commit()?;
    Ok(changed)
}

pub(crate) fn apply_palette(library: &Library, asset_id: &str, fingerprint: &str, metadata: &Value, palette: &SubjectPalette) -> CoreResult<bool> {
    if !library.auto_tag_settings()?.colors { return Ok(false); }
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    let current: (String, String, String) = transaction.query_row(
        "SELECT a.fingerprint,a.asset_type,m.value_json FROM assets a JOIN metadata m ON m.asset_id=a.id AND m.key='parsed' WHERE a.id=?1",
        [asset_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if current.0 != fingerprint || serde_json::from_str::<Value>(&current.2)? != *metadata {
        return Err(CoreError::Card("资产在颜色分析期间发生变化，请重新生成".to_owned()));
    }
    if current.1 != "model" { return Ok(false); }
    let user_palette: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM asset_tags at JOIN tags t ON at.tag_id=t.id WHERE at.asset_id=?1 AND at.source='user' AND t.name LIKE '整体色:%')",
        [asset_id], |row| row.get(0),
    )?;
    let missing_texture = metadata["file_dependencies"].as_array().is_some_and(|dependencies| dependencies.iter().any(|dependency| dependency["status"] == "missing"));
    let candidates = if !user_palette && !missing_texture && palette.status == "ready" && palette.algorithm_version == REVISION {
        let mut seen = HashSet::new();
        palette.colors.iter().filter(|color| color.share.is_finite() && (0.18..=1.0).contains(&color.share)
            && matches!(color.name.as_str(), "黑色" | "白色" | "灰色" | "红色" | "橙色" | "黄色" | "绿色" | "青色" | "蓝色" | "紫色" | "粉色" | "棕色"))
            .filter(|color| seen.insert(color.name.as_str())).take(3).map(|color| format!("整体色:{}", color.name)).collect::<Vec<_>>()
    } else { Vec::new() };
    let changed = reconcile(&transaction, asset_id, "palette", fingerprint, &candidates, 0.78)?;
    transaction.commit()?;
    Ok(changed)
}

// Analyse the native character thumbnail before lossy WebP encoding. Background,
// near-background edges and tiny accents are excluded; this describes visible
// overall colours, never hair/clothing regions or the model's identity.
pub(crate) fn subject_palette(rgba: &[u8], width: u32, height: u32, diagnostics: &[String]) -> SubjectPalette {
    let mut palette = SubjectPalette { algorithm_version: REVISION, status: "insufficient_subject".to_owned(), sampled_pixels: 0, colors: Vec::new() };
    if diagnostics.iter().any(|value| value.starts_with("MissingTexture:") || value.starts_with("TextureLoadFailed:")
        || value.contains("TextureBudgetExceeded:") || value.contains("TextureMemoryBudgetExceeded:") || value.starts_with("MissingSphereTexture:")
        || value.starts_with("SphereTextureLoadFailed:")) {
        palette.status = "texture_unavailable".to_owned(); return palette;
    }
    let Some(pixels) = (width as usize).checked_mul(height as usize) else { return palette; };
    if width < 8 || height < 8 || pixels.checked_mul(4) != Some(rgba.len()) { return palette; }
    let corners = [0, width as usize - 1, pixels - width as usize, pixels - 1];
    let background = &rgba[..3];
    if corners.iter().any(|index| (0..3).any(|channel| rgba[index * 4 + channel].abs_diff(background[channel]) > 4)) {
        palette.status = "background_uncertain".to_owned(); return palette;
    }
    const NAMES: [&str; 12] = ["黑色", "白色", "灰色", "红色", "橙色", "黄色", "绿色", "青色", "蓝色", "紫色", "粉色", "棕色"];
    let mut counts = [0usize; 12];
    for y in (2..height as usize - 2).step_by(4) { for x in (2..width as usize - 2).step_by(4) {
        let pixel = &rgba[(y * width as usize + x) * 4..][..4];
        if pixel[3] < 224 || (0..3).all(|channel| pixel[channel].abs_diff(background[channel]) < 14) { continue; }
        // Erode antialiased silhouette edges instead of counting background mixtures.
        if [x - 1, x + 1].iter().any(|neighbor| (0..3).all(|channel| rgba[(y * width as usize + neighbor) * 4 + channel].abs_diff(background[channel]) < 8)) { continue; }
        counts[color_bin([pixel[0], pixel[1], pixel[2]])] += 1;
        palette.sampled_pixels += 1;
    } }
    if palette.sampled_pixels < 300 { return palette; }
    let mut ranked: Vec<_> = counts.into_iter().enumerate().collect();
    ranked.sort_by_key(|&(index, count)| (std::cmp::Reverse(count), index));
    palette.colors = ranked.into_iter().take(3).filter_map(|(index, count)| {
        let share = count as f64 / palette.sampled_pixels as f64;
        (share >= 0.18).then(|| SubjectColor { name: NAMES[index].to_owned(), share })
    }).collect();
    palette.status = if palette.colors.iter().map(|color| color.share).sum::<f64>() >= 0.55 { "ready" } else { "mixed" }.to_owned();
    palette
}

fn color_bin(rgb: [u8; 3]) -> usize {
    let [r, g, b] = rgb.map(|value| value as f64 / 255.0);
    let max = r.max(g).max(b); let min = r.min(g).min(b); let delta = max - min;
    if max < 0.20 { return 0; }
    let saturation = delta / max;
    if saturation < 0.16 { return if min > 0.76 { 1 } else { 2 }; }
    let hue = (if max == r { (g - b) / delta } else if max == g { (b - r) / delta + 2.0 } else { (r - g) / delta + 4.0 } * 60.0).rem_euclid(360.0);
    if !(15.0..345.0).contains(&hue) { return if max > 0.70 && saturation < 0.65 { 10 } else { 3 }; }
    if hue < 45.0 { return if max < 0.60 { 11 } else { 4 }; }
    if hue < 70.0 { return 5; } if hue < 165.0 { return 6; } if hue < 200.0 { return 7; }
    if hue < 260.0 { return 8; } if hue < 320.0 { return 9; } 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_ignores_background_accents_and_missing_textures() {
        let mut image = vec![0u8; 128 * 128 * 4];
        for pixel in image.chunks_exact_mut(4) { pixel.copy_from_slice(&[66, 77, 85, 255]); }
        for y in 16..112 { for x in 30..98 { image[(y * 128 + x) * 4..][..4].copy_from_slice(&[30, 85, 220, 255]); } }
        for y in 50..54 { for x in 50..54 { image[(y * 128 + x) * 4..][..4].copy_from_slice(&[240, 20, 20, 255]); } }
        let palette = subject_palette(&image, 128, 128, &[]);
        assert_eq!(palette.status, "ready"); assert_eq!(palette.colors.len(), 1); assert_eq!(palette.colors[0].name, "蓝色");
        assert_eq!(subject_palette(&image, 128, 128, &["MissingTexture:coat.png".to_owned()]).status, "texture_unavailable");
        assert_eq!(subject_palette(&vec![66; 128 * 128 * 4], 128, 128, &[]).status, "insufficient_subject");
    }

    #[test]
    fn reconciliation_is_idempotent_and_keeps_user_and_agent_assignments() {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch("INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at) VALUES ('r','model','/test','/test','test','now'); INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,statuses_json,created_at,updated_at,last_seen_at) VALUES ('a','r','model','test','/test/a.pmx','/test','f','[]','now','now','now');").unwrap();
        let run = |names: &[&str]| {
            let mut connection = library.connection().unwrap(); let tx = connection.transaction().unwrap();
            let changed = reconcile(&tx, "a", "technical", "f", &names.iter().map(|name| name.to_string()).collect::<Vec<_>>(), 1.0).unwrap();
            tx.commit().unwrap(); changed
        };
        assert!(run(&["格式:PMX", "技术:含SDEF"])); assert!(!run(&["格式:PMX", "技术:含SDEF"]));
        library.add_asset_tag("a", "技术:含SDEF", "user", None).unwrap();
        library.add_asset_tag("a", "服装:裙装", "agent", Some(0.85)).unwrap();
        assert!(run(&["格式:PMD"]));
        let tags = library.list_asset_tags("a").unwrap();
        assert!(tags.iter().any(|tag| tag.name == "技术:含SDEF" && tag.source == "user"));
        assert!(tags.iter().any(|tag| tag.name == "服装:裙装" && tag.source == "agent"));
        assert!(!tags.iter().any(|tag| tag.name == "格式:PMX"));
        library.remove_asset_tag("a", "格式:PMD").unwrap(); assert!(!run(&["格式:PMD"]));
        assert!(!library.list_asset_tags("a").unwrap().iter().any(|tag| tag.name == "格式:PMD"));
    }

    #[test]
    fn facts_do_not_guess_costumes_or_motion_content() {
        let model = technical_tags(AssetType::Model, &serde_json::json!({"file_type":"pmx", "rigid_body_count":3,"usable_morph_count":2,"sdef_vertex_count":1,"skeleton_class":"nonstandard","name":"pleated skirt blue"}));
        assert!(model.contains(&"技术:含物理".to_owned())); assert!(model.contains(&"技术:含SDEF".to_owned()));
        assert!(!model.iter().any(|name| name.starts_with("裙型:") || name.starts_with("整体色:")));
        let motion = technical_tags(AssetType::Motion, &serde_json::json!({"file_type":"vmd","has_bone_motion":true,"is_pose":false,"name":"dance"}));
        assert!(motion.contains(&"轨道:含骨骼".to_owned())); assert!(!motion.contains(&"动作:舞蹈".to_owned()));
        assert!(technical_tags(AssetType::Motion, &serde_json::json!({"file_type":"vmd","is_camera_only":true})).is_empty());
    }
}
