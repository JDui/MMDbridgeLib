use std::path::Path;

use mmd_anim_format::{parse_pmd_model, parse_pmx_model, parse_vmd_animation, parse_vpd_pose};
use serde_json::json;

use crate::types::{AssetType, ParsedCandidate, ParsedDependency, display_name};

const CAMERA_CLASSIFICATION_VERSION: u8 = 2;
const PMX_PARSER_REVISION: u32 = 2;
const PMD_PARSER_REVISION: u32 = 1;
const VMD_PARSER_REVISION: u32 = 2;
const VPD_PARSER_REVISION: u32 = 1;

pub(crate) fn revision_for_path(path: &Path) -> u32 {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pmx" => PMX_PARSER_REVISION,
        "pmd" => PMD_PARSER_REVISION,
        "vmd" => VMD_PARSER_REVISION,
        "vpd" => VPD_PARSER_REVISION,
        _ => 0,
    }
}

pub(crate) fn parse_asset(
    asset_type: AssetType,
    path: &Path,
    bytes: &[u8],
) -> Result<ParsedCandidate, String> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    let fallback_name = display_name(path);
    match asset_type {
        AssetType::Model => {
            let parsed = parse_pmx_model(bytes).map_err(|error| error.to_string())?;
            Ok(ParsedCandidate {
                name: non_empty(&parsed.metadata.name, fallback_name),
                metadata: json!({
                    "file_type": "pmx", "pmx_version": parsed.metadata.version,
                    "vertex_count": parsed.metadata.counts.vertices, "polygon_count": parsed.metadata.counts.faces,
                    "material_count": parsed.metadata.counts.materials, "bone_count": parsed.metadata.counts.bones,
                    "morph_count": parsed.metadata.counts.morphs, "rigid_body_count": parsed.metadata.counts.rigid_bodies,
                    "joint_count": parsed.metadata.counts.joints, "english_name": parsed.metadata.english_name,
                    "parser_diagnostics": parsed.diagnostics,
                    "skeleton_class": if is_standard_mmd_skeleton(&parsed) { "standard" } else { "nonstandard" },
                }),
                status: "Ready".to_owned(),
                dependencies: pmx_dependencies(&parsed.materials),
            })
        }
        AssetType::Motion if extension.eq_ignore_ascii_case("vmd") => {
            let parsed = parse_vmd_animation(bytes).map_err(|error| error.to_string())?;
            let frames = parsed
                .bone_frames
                .iter()
                .map(|key| key.frame)
                .chain(parsed.morph_frames.iter().map(|key| key.frame))
                .chain(parsed.camera_frames.iter().map(|key| key.frame))
                .chain(parsed.light_frames.iter().map(|key| key.frame))
                .chain(parsed.property_frames.iter().map(|key| key.frame))
                .chain(parsed.self_shadow_frames.iter().map(|key| key.frame));
            let (mut min_frame, mut max_frame) = (None, None);
            for frame in frames {
                min_frame = Some(min_frame.map_or(frame, |current: u32| current.min(frame)));
                max_frame = Some(max_frame.map_or(frame, |current: u32| current.max(frame)));
            }
            let has_bone_motion = !parsed.bone_frames.is_empty();
            let has_morph_motion = !parsed.morph_frames.is_empty();
            let has_camera = !parsed.camera_frames.is_empty();
            let has_light = !parsed.light_frames.is_empty();
            let has_ik = parsed
                .property_frames
                .iter()
                .any(|frame| !frame.ik_states.is_empty());
            let start_frame = min_frame.unwrap_or(0);
            let end_frame = max_frame.unwrap_or(0);
            let total_frames = min_frame.map_or(0, |_| {
                end_frame.saturating_sub(start_frame).saturating_add(1)
            });
            let model_motion_is_single_frame = !has_multiple_frames(
                parsed
                    .bone_frames
                    .iter()
                    .map(|key| key.frame)
                    .chain(parsed.morph_frames.iter().map(|key| key.frame)),
            );
            let has_camera_animation =
                has_multiple_frames(parsed.camera_frames.iter().map(|key| key.frame));
            let has_light_animation =
                has_multiple_frames(parsed.light_frames.iter().map(|key| key.frame));
            let has_property_animation =
                has_multiple_frames(parsed.property_frames.iter().map(|key| key.frame));
            let has_self_shadow_animation =
                has_multiple_frames(parsed.self_shadow_frames.iter().map(|key| key.frame));
            let is_pose = (has_bone_motion || has_morph_motion)
                && model_motion_is_single_frame
                && !has_camera_animation
                && !has_light_animation
                && !has_property_animation
                && !has_self_shadow_animation;
            let is_camera_only =
                has_camera
                    && !has_bone_motion
                    && !has_morph_motion
                    && !has_light
                    && parsed.property_frames.is_empty()
                    && parsed.self_shadow_frames.is_empty();
            Ok(ParsedCandidate {
                name: fallback_name,
                metadata: json!({
                    "file_type": "vmd", "start_frame": start_frame, "end_frame": end_frame,
                    "total_frames": total_frames, "duration_seconds": total_frames as f64 / 30.0,
                    "has_bone_motion": has_bone_motion, "has_morph_motion": has_morph_motion,
                    "has_camera": has_camera, "has_light": has_light, "has_ik": has_ik,
                    "has_self_shadow": !parsed.self_shadow_frames.is_empty(), "is_camera_only": is_camera_only,
                    "camera_classification_version": CAMERA_CLASSIFICATION_VERSION,
                    "is_pose": is_pose, "preview_frame": 0, "vmd_model_name": parsed.metadata.model_name,
                    "key_counts": parsed.metadata.counts,
                }),
                status: "Ready".to_owned(),
                dependencies: Vec::new(),
            })
        }
        AssetType::Motion if extension.eq_ignore_ascii_case("vpd") => {
            let parsed = parse_vpd_pose(bytes).map_err(|error| error.to_string())?;
            Ok(ParsedCandidate {
                name: fallback_name,
                metadata: json!({"file_type":"vpd", "is_pose":true, "bone_count":parsed.bone_count, "model_file":parsed.model_file, "parser_diagnostics":parsed.diagnostics}),
                status: "Ready".to_owned(),
                dependencies: Vec::new(),
            })
        }
        AssetType::Scene if extension.eq_ignore_ascii_case("pmx") => {
            let parsed = parse_pmx_model(bytes).map_err(|error| error.to_string())?;
            let (width, depth) = bounds_xz(
                parsed
                    .geometry
                    .positions
                    .chunks_exact(3)
                    .map(|p| [p[0], p[1], p[2]]),
            );
            Ok(ParsedCandidate {
                name: non_empty(&parsed.metadata.name, fallback_name),
                metadata: json!({"file_type":"pmx", "polygon_count":parsed.metadata.counts.faces, "width":width, "depth":depth, "area":width*depth, "coordinate_unit":"MMD", "parser_diagnostics":parsed.diagnostics}),
                status: "Ready".to_owned(),
                dependencies: pmx_dependencies(&parsed.materials),
            })
        }
        AssetType::Scene if extension.eq_ignore_ascii_case("pmd") => {
            let parsed = parse_pmd_model(bytes).map_err(|error| error.to_string())?;
            let (width, depth) = bounds_xz(
                parsed
                    .geometry
                    .vertices
                    .iter()
                    .map(|vertex| vertex.position),
            );
            let mut dependencies = Vec::new();
            for material in &parsed.materials {
                let references = material
                    .texture_name
                    .split('*')
                    .map(str::trim)
                    .filter(|reference| !reference.is_empty());
                for (index, reference) in references.enumerate() {
                    push_dependency(
                        &mut dependencies,
                        reference,
                        if index == 0 {
                            "texture"
                        } else {
                            "sphere_texture"
                        },
                    );
                }
            }
            for reference in &parsed.toon_textures {
                push_dependency(&mut dependencies, reference, "shared_toon_texture");
            }
            Ok(ParsedCandidate {
                name: non_empty(&parsed.metadata.name, fallback_name),
                metadata: json!({"file_type":"pmd", "polygon_count":parsed.metadata.counts.faces, "width":width, "depth":depth, "area":width*depth, "coordinate_unit":"MMD", "bone_count":parsed.metadata.counts.bones, "parser_diagnostics":parsed.diagnostics}),
                status: "Ready".to_owned(),
                dependencies,
            })
        }
        _ => Err(format!(
            "unsupported extension for {} root",
            asset_type.as_str()
        )),
    }
}

fn pmx_dependencies(
    materials: &[mmd_anim_format::pmx::PmxParsedMaterial],
) -> Vec<ParsedDependency> {
    let mut dependencies = Vec::new();
    for material in materials {
        push_dependency(&mut dependencies, &material.texture_path, "texture");
        push_dependency(
            &mut dependencies,
            &material.sphere_texture_path,
            "sphere_texture",
        );
        push_dependency(
            &mut dependencies,
            &material.toon_texture_path,
            "toon_texture",
        );
    }
    dependencies
}

fn push_dependency(dependencies: &mut Vec<ParsedDependency>, reference: &str, role: &str) {
    let reference = reference.trim();
    if reference.is_empty()
        || dependencies
            .iter()
            .any(|dependency| dependency.reference == reference && dependency.role == role)
    {
        return;
    }
    dependencies.push(dependency(reference, role));
}

fn dependency(reference: &str, role: &str) -> ParsedDependency {
    ParsedDependency {
        reference: reference.trim().to_owned(),
        role: role.to_owned(),
        path: None,
        status: "unresolved".to_owned(),
    }
}

// Extra twist/IK bones are allowed; body anchors must form connected MMD chains.
fn is_standard_mmd_skeleton(model: &mmd_anim_format::PmxParsedModel) -> bool {
    let bones = &model.skeleton.bones;
    let find = |aliases: &[&str]| bones.iter().position(|bone| {
        [&bone.name, &bone.english_name].iter().any(|name| {
            let normalized = name.to_lowercase().replace([' ', '_', '-'], "");
            aliases.contains(&normalized.as_str())
        })
    });
    let chains: &[&[&[&str]]] = &[
        &[&["センター", "center"], &["上半身", "upperbody"], &["頭", "head"]],
        &[&["上半身", "upperbody"], &["左腕", "leftarm", "arml"], &["左ひじ", "左肘", "leftelbow", "elbowl"], &["左手首", "leftwrist", "wristl"]],
        &[&["上半身", "upperbody"], &["右腕", "rightarm", "armr"], &["右ひじ", "右肘", "rightelbow", "elbowr"], &["右手首", "rightwrist", "wristr"]],
        &[&["センター", "center"], &["下半身", "lowerbody"], &["左足", "leftleg", "legl"], &["左ひざ", "左膝", "leftknee", "kneel"], &["左足首", "leftankle", "anklel"]],
        &[&["センター", "center"], &["下半身", "lowerbody"], &["右足", "rightleg", "legr"], &["右ひざ", "右膝", "rightknee", "kneer"], &["右足首", "rightankle", "ankler"]],
    ];
    chains.iter().all(|chain| {
        let Some(indices) = chain.iter().map(|names| find(names)).collect::<Option<Vec<_>>>() else { return false; };
        indices.windows(2).all(|pair| {
            let mut child = pair[1];
            for _ in 0..bones.len() {
                let Ok(parent) = usize::try_from(bones[child].parent_index) else { return false; };
                if parent >= bones.len() || parent == child { return false; }
                if parent == pair[0] { return true; }
                child = parent;
            }
            false
        })
    })
}

fn non_empty(value: &str, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod preview_probe {
    use super::*;

    #[test]
    #[ignore = "read-only PMX probe: requires MMDBRIDGE_PROBE_SOURCE and MMDBRIDGE_PROBE_OUTPUT"]
    fn readonly_model_thumbnail() {
        let path = std::path::PathBuf::from(std::env::var_os("MMDBRIDGE_PROBE_SOURCE").unwrap());
        let output = std::path::PathBuf::from(std::env::var_os("MMDBRIDGE_PROBE_OUTPUT").unwrap());
        let bytes = std::fs::read(&path).unwrap();
        let model = parse_pmx_model(&bytes).unwrap();
        let candidate = parse_asset(AssetType::Model, &path, &bytes).unwrap();
        println!("classification: {}", candidate.metadata["skeleton_class"]);
        let mut invalid = model.clone();
        invalid.skeleton.bones.clear();
        assert!(!is_standard_mmd_skeleton(&invalid));
        let library = crate::Library::in_memory().unwrap();
        let preview = library.render_thumbnail_file(&path).unwrap();
        println!("{}", serde_json::to_string_pretty(&preview.report).unwrap());
        std::fs::write(output, preview.preview_webp).unwrap();
    }
}

fn has_multiple_frames(frames: impl Iterator<Item = u32>) -> bool {
    let mut frames = frames;
    let Some(first) = frames.next() else {
        return false;
    };
    frames.any(|frame| frame != first)
}

fn bounds_xz(positions: impl Iterator<Item = [f32; 3]>) -> (f64, f64) {
    let (mut min_x, mut max_x, mut min_z, mut max_z) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for [x, _, z] in positions {
        if x.is_finite() && z.is_finite() {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_z = min_z.min(z);
            max_z = max_z.max(z);
        }
    }
    if min_x.is_finite() && min_z.is_finite() {
        (f64::from(max_x - min_x), f64::from(max_z - min_z))
    } else {
        (0.0, 0.0)
    }
}
