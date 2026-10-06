use std::{path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}, time::SystemTime};

use mmd_anim_format::vmd::{self, VmdParsedCameraFrame};
use mmd_anim_runtime::{AnimationClip, RuntimeInstance};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::{AssetType, CoreError, CoreResult, Library};
use crate::model_io::import_model_runtime;

const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
static SESSION: OnceLock<Mutex<Option<MotionViewSession>>> = OnceLock::new();

struct MotionViewSession {
    motion_path: PathBuf,
    motion_modified: Option<SystemTime>,
    model_path: PathBuf,
    model_modified: Option<SystemTime>,
    paired_path: Option<PathBuf>,
    paired_modified: Option<SystemTime>,
    clip: AnimationClip,
    runtime: RuntimeInstance,
    own_camera: Vec<VmdParsedCameraFrame>,
    paired_camera: Vec<VmdParsedCameraFrame>,
    max_frame: u32,
}

fn modified(path: &Path) -> CoreResult<Option<SystemTime>> {
    Ok(std::fs::metadata(path)?.modified().ok())
}

fn read_limited(path: &Path) -> CoreResult<Vec<u8>> {
    if std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
        return Err(CoreError::ModelPreview(format!("文件超过 512 MiB 预览上限：{}", path.display())));
    }
    Ok(crate::model_io::read_source(path)?)
}

fn paired_camera_path(library: &Library, asset_id: &str) -> CoreResult<Option<PathBuf>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT camera.primary_source FROM relations r JOIN assets camera ON camera.id=r.target_asset
         LEFT JOIN metadata camera_metadata ON camera_metadata.asset_id=camera.id AND camera_metadata.key='parsed'
         WHERE r.relation_type='MotionCameraPair' AND r.source_asset=?1
           AND camera.retired_format=0 AND instr(camera.statuses_json,'MissingSource')=0
           AND json_extract(CASE WHEN json_valid(camera_metadata.value_json) THEN camera_metadata.value_json ELSE '{}' END,'$.has_camera')=1
         ORDER BY r.confirmed DESC,r.confidence DESC LIMIT 1"
    )?;
    let result = statement.query_row([asset_id], |row| row.get::<_, String>(0))
        .optional()?;
    Ok(result.map(PathBuf::from))
}

fn create_session(motion_path: PathBuf, model_path: PathBuf, paired_path: Option<PathBuf>) -> CoreResult<MotionViewSession> {
    let motion_modified = modified(&motion_path)?;
    let model_modified = modified(&model_path)?;
    let paired_modified = paired_path.as_deref().map(modified).transpose()?.flatten();
    let vmd = vmd::parse_vmd_shared_context(&read_limited(&motion_path)?)
        .map_err(|error| CoreError::ModelPreview(format!("VMD 解析失败：{error}")))?;
    let animation = vmd.import_result().clone();
    let own_camera = vmd.parsed_animation().camera_frames.clone();
    let paired_camera = if let Some(path) = paired_path.as_deref() {
        let paired = vmd::parse_vmd_shared_context(&read_limited(path)?)
            .map_err(|error| CoreError::ModelPreview(format!("配套镜头解析失败：{error}")))?;
        paired.parsed_animation().camera_frames.clone()
    } else { Vec::new() };
    let max_frame = animation.bone_keyframes.iter().map(|frame| frame.frame)
        .chain(animation.morph_keyframes.iter().map(|(_, frame, _)| *frame))
        .chain(own_camera.iter().map(|frame| frame.frame))
        .chain(paired_camera.iter().map(|frame| frame.frame))
        .max().unwrap_or(0);
    let imported = import_model_runtime(&read_limited(&model_path)?)
        .map_err(|error| CoreError::ModelPreview(format!("模型骨架解析失败：{error}")))?;
    let clip = vmd::build_pair_clip(
        &animation,
        &imported.bone_name_to_index,
        &imported.morph_name_to_index,
        &imported.ik_solver_bone_name_to_index,
        imported.model.ik_solvers().len(),
    );
    let mut runtime = RuntimeInstance::new(Arc::new(imported.model));
    runtime.evaluate_rest_pose();
    Ok(MotionViewSession { motion_path, motion_modified, model_path, model_modified, paired_path, paired_modified,
        clip, runtime, own_camera, paired_camera, max_frame })
}

pub(crate) fn frame(library: &Library, asset_id: &str, requested_frame: u32) -> CoreResult<Value> {
    let asset = library.inspect_asset(asset_id)?;
    if asset.asset_type != AssetType::Motion || !asset.primary_source.to_ascii_lowercase().ends_with(".vmd") {
        return Err(CoreError::ModelPreview("3D 动作预览需要 VMD 资产".to_owned()));
    }
    let model_path = library.motion_preview_model()?.ok_or_else(||
        CoreError::ModelPreview("请先设置动作预览模型 PMX / PMD".to_owned()))?;
    let motion_path = PathBuf::from(&asset.primary_source);
    let model_path = PathBuf::from(model_path);
    let paired_path = paired_camera_path(library, asset_id)?;
    let session_lock = SESSION.get_or_init(|| Mutex::new(None));
    let mut guard = session_lock.lock().map_err(|_| CoreError::LockPoisoned)?;
    let reload = match guard.as_ref() {
        Some(session) => session.motion_path != motion_path || session.model_path != model_path
            || session.paired_path != paired_path || session.motion_modified != modified(&motion_path)?
            || session.model_modified != modified(&model_path)?
            || session.paired_modified != paired_path.as_deref().map(modified).transpose()?.flatten(),
        None => true,
    };
    if reload { *guard = Some(create_session(motion_path, model_path, paired_path)?); }
    let session = guard.as_mut().expect("motion session exists");
    let frame = requested_frame.min(session.max_frame);
    let sample = session.clip.sample_at(frame as f32);
    sample.apply_to_pose(session.runtime.pose_mut());
    session.runtime.expand_morphs();
    session.runtime.evaluate_current_pose();
    let matrices = session.runtime.pose().world_matrices().iter()
        .flat_map(|matrix| matrix.to_cols_array()).collect::<Vec<_>>();
    let own_camera = vmd::sample_vmd_camera_frames(&session.own_camera, frame as f32);
    let paired_camera = vmd::sample_vmd_camera_frames(&session.paired_camera, frame as f32);
    Ok(json!({"frame":frame,"maxFrame":session.max_frame,"modelPath":session.model_path,
        "boneMatrices":matrices,"ownCamera":own_camera,"pairedCamera":paired_camera,
        "hasPairedCamera":!session.paired_camera.is_empty()}))
}
