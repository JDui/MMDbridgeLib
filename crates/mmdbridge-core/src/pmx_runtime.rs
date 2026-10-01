use mmd_anim_format::{PmxRuntimeImport, export_pmx_model, parse_pmx_model};
use mmd_anim_format::error::ImportError;
use mmd_anim_runtime::ModelBuildError;

#[derive(Default)]
pub(crate) struct PmxRuntimeCorrections {
    pub zero_rotations: usize,
    pub negative_group_references: usize,
    pub self_group_references: usize,
    pub self_bone_parents: usize,
}

/// Preserve successful imports; repair known malformed references and optional
/// Morph records in memory through the existing PMX codec.
pub(crate) fn import_pmx_runtime_compatible(
    bytes: &[u8],
) -> Result<(PmxRuntimeImport, PmxRuntimeCorrections), ImportError> {
    let original_error = match mmd_anim_format::import_pmx_runtime(bytes) {
        Ok(imported) => return Ok((imported, PmxRuntimeCorrections::default())),
        Err(error) => error,
    };
    if !matches!(original_error, ImportError::SectionOverflow) && !matches!(&original_error,
        ImportError::ModelBuildFailed(ModelBuildError::InvalidRuntimeDescriptor { path, reason })
        if path.starts_with("morphs.bone_offsets[") && path.ends_with(".rotation_offset")
            && reason.contains("quaternion")) && !matches!(&original_error,
        ImportError::ModelBuildFailed(ModelBuildError::InvalidRuntimeDescriptor { path, reason })
        if path.starts_with("morphs.group_offsets[") && reason.contains("cycle")) && !matches!(&original_error,
        ImportError::ModelBuildFailed(ModelBuildError::InvalidRuntimeDescriptor { path, reason })
        if path.starts_with("bones[") && path.ends_with(".parent")
            && reason == "parent cannot reference itself") {
        return Err(original_error);
    }
    let mut model = parse_pmx_model(bytes)?;
    let mut corrected = PmxRuntimeCorrections::default();
    for (index, bone) in model.skeleton.bones.iter_mut().enumerate() {
        if bone.parent_index == index as i32 {
            bone.parent_index = -1;
            corrected.self_bone_parents += 1;
        }
    }
    for (index, morph) in model.morphs.iter_mut().enumerate() {
        let before = morph.group_offsets.len();
        morph.group_offsets.retain(|offset| offset.morph_index >= 0);
        corrected.negative_group_references += before - morph.group_offsets.len();
        let before = morph.group_offsets.len();
        morph.group_offsets.retain(|offset| offset.morph_index != index as i32);
        corrected.self_group_references += before - morph.group_offsets.len();
        for offset in &mut morph.bone_offsets {
            if offset.rotation == [0.0; 4] {
                offset.rotation = [0.0, 0.0, 0.0, 1.0];
                corrected.zero_rotations += 1;
            }
        }
    }
    if corrected.zero_rotations == 0 && corrected.negative_group_references == 0
        && corrected.self_group_references == 0 && corrected.self_bone_parents == 0 {
        return Err(original_error);
    }
    mmd_anim_format::import_pmx_runtime(&export_pmx_model(&model))
        .map(|imported| (imported, corrected))
}
