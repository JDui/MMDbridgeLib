use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, mpsc},
    time::Duration,
};

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3};
use image::{ImageReader, imageops::FilterType};
use mmd_anim_format::{
    AccessoryParsedManifest, PmdParsedModel, PmxParsedModel, VpdParsedPose, import_pmx_runtime,
    parse_accessory_manifest, parse_pmd_model, parse_pmx_model, parse_vpd_pose,
};
use mmd_anim_runtime::{BoneIndex, ClipSample, MorphIndex, RuntimeInstance};
use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

use crate::thumbnail_concurrency::{self, ThumbnailStage};
use crate::{CoreError, CoreResult};

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 1024;
const QUALITY: f32 = 50.0;
pub(crate) const RENDERER_VERSION: &str = "0.5.0";
pub(crate) const PREVIEW_SETTINGS_VERSION: &str = "front-minus-z-posed-frame-soft-matcap-v6";
pub(crate) const SCENE_PREVIEW_SETTINGS_VERSION: &str = "scene-center-165cm-wide-camera-soft-matcap-v2";
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TEXTURE_DIMENSION: u32 = 4096;
const MAX_TEXTURE_DECODE_DIMENSION: u32 = 8192;
const MAX_TEXTURE_SOURCE_BYTES: u64 = 128 * 1024 * 1024;

static RENDERER: OnceLock<Result<GpuRenderer, String>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailRenderReport {
    pub renderer_version: String,
    #[serde(default)]
    pub preview_settings_version: String,
    pub adapter: String,
    pub front_axis: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub quality: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_frame: Option<u32>,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub material_count: usize,
    pub texture_count: usize,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GeneratedThumbnail {
    pub preview_webp: Vec<u8>,
    pub report: ThumbnailRenderReport,
}

#[derive(Clone, Copy)]
struct SkinnedVertex {
    position: Vec3,
    normal: Vec3,
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 4],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform {
    diffuse: [f32; 4],
    ambient: [f32; 4],
    specular: [f32; 4],
    texture_factor: [f32; 4],
    sphere_factor: [f32; 4],
    toon_factor: [f32; 4],
    flags: [u32; 4],
}

struct TextureResource {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct TextureData {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    alpha: TextureAlpha,
}

#[derive(Clone, Copy, Default)]
struct TextureAlpha {
    masked: bool,
    blended: bool,
}

struct DrawGroup {
    start: u32,
    end: u32,
    material_index: usize,
    depth: f32,
    transparent: bool,
}

struct MaterialRange {
    start: usize,
    count: usize,
    material_index: usize,
}

struct RenderMaterial {
    name: String,
    texture_path: String,
    sphere_texture_path: String,
    toon_texture_path: String,
    shared_toon_index: Option<u8>,
    sphere_mode: u32,
    diffuse: [f32; 4],
    ambient: [f32; 4],
    specular: [f32; 4],
    texture_factor: [f32; 4],
    sphere_factor: [f32; 4],
    toon_factor: [f32; 4],
    toon_enabled: bool,
    vertex_color_mode: u32,
}

struct RenderInput {
    vertices: Vec<SkinnedVertex>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    material_ranges: Vec<MaterialRange>,
    materials: Vec<RenderMaterial>,
    camera: Option<mmd_anim_format::vmd::VmdCameraState>,
    scene_view: bool,
    framing_bounds: Option<(Vec3, Vec3)>,
    diagnostics: Vec<String>,
}

struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_name: String,
    opaque_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
    material_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    toon_sampler: wgpu::Sampler,
}

pub(crate) fn render_file(path: &Path) -> CoreResult<GeneratedThumbnail> {
    let mut no_progress = |_: &str, _: f64| true;
    render_file_with_progress(path, false, &mut no_progress)
}

pub(crate) fn render_file_with_progress(
    path: &Path,
    scene_asset: bool,
    progress: &mut dyn FnMut(&str, f64) -> bool,
) -> CoreResult<GeneratedThumbnail> {
    let parse_permit =
        thumbnail_concurrency::acquire(ThumbnailStage::Parse, || progress("Parsing", 0.05))?;
    if !progress("Parsing", 0.05) {
        return Err(CoreError::ThumbnailCancelled);
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(CoreError::ThumbnailRender(
            "模型文件超过 512 MiB 渲染上限".to_owned(),
        ));
    }
    let bytes = std::fs::read(path)?;
    let mut input = if extension.eq_ignore_ascii_case("pmx") {
        let model = parse_pmx_model(&bytes)
            .map_err(|error| CoreError::ThumbnailRender(format!("PMX 解析失败：{error}")))?;
        if !progress("Parsing", 0.20) {
            return Err(CoreError::ThumbnailCancelled);
        }
        render_input_from_pmx(&bytes, &model)?
    } else if extension.eq_ignore_ascii_case("pmd") {
        let model = parse_pmd_model(&bytes)
            .map_err(|error| CoreError::ThumbnailRender(format!("PMD 解析失败：{error}")))?;
        if !progress("Parsing", 0.20) {
            return Err(CoreError::ThumbnailCancelled);
        }
        render_input_from_pmd(&model)?
    } else if extension.eq_ignore_ascii_case("x") {
        let file_name = path.file_name().and_then(|name| name.to_str());
        let manifest = if crate::x_binary::is_binary_x(&bytes) {
            crate::x_binary::parse_binary_x(&bytes)
                .map_err(|error| CoreError::ThumbnailRender(format!("X 场景解析失败：{error}")))?
        } else {
            parse_accessory_manifest(&bytes, file_name)
                .map_err(|error| CoreError::ThumbnailRender(format!("X 场景解析失败：{error}")))?
        };
        if !progress("Parsing", 0.20) {
            return Err(CoreError::ThumbnailCancelled);
        }
        render_input_from_x(&manifest)?
    } else {
        return Err(CoreError::ThumbnailRender(format!(
            "当前缩略图渲染器不支持 .{} 文件",
            extension.to_ascii_lowercase()
        )));
    };
    if scene_asset {
        input.scene_view = true;
        input.framing_bounds = None;
    }
    drop(parse_permit);
    if !progress("Rendering", 0.40) {
        return Err(CoreError::ThumbnailCancelled);
    }
    let renderer = RENDERER
        .get_or_init(|| pollster::block_on(GpuRenderer::new()).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| CoreError::ThumbnailRender(error.clone()))?;
    renderer.render(path, input, progress)
}

pub(crate) fn motion_preview_settings_version(model_path: &Path) -> CoreResult<String> {
    let metadata = std::fs::metadata(model_path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(format!(
        "{PREVIEW_SETTINGS_VERSION}:motion-camera-v3:{}:{}:{modified}",
        model_path.to_string_lossy(),
        metadata.len()
    ))
}

fn scene_view_input(path: &Path) -> CoreResult<RenderInput> {
    if std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
        return Err(CoreError::ModelPreview("场景超过 512 MiB 预览上限".to_owned()));
    }
    let bytes = std::fs::read(path)?;
    match path.extension().and_then(|extension| extension.to_str()).unwrap_or_default().to_ascii_lowercase().as_str() {
        "pmx" => {
            let model = parse_pmx_model(&bytes).map_err(|error| CoreError::ModelPreview(error.to_string()))?;
            render_input_from_pmx(&bytes, &model)
        }
        "pmd" => {
            let model = parse_pmd_model(&bytes).map_err(|error| CoreError::ModelPreview(error.to_string()))?;
            render_input_from_pmd(&model)
        }
        "x" => {
            let manifest = if crate::x_binary::is_binary_x(&bytes) {
                crate::x_binary::parse_binary_x(&bytes)
                    .map_err(|error| CoreError::ModelPreview(error.to_string()))?
            } else {
                parse_accessory_manifest(&bytes, path.file_name().and_then(|name| name.to_str()))
                    .map_err(|error| CoreError::ModelPreview(error.to_string()))?
            };
            render_input_from_x(&manifest)
        }
        _ => Err(CoreError::ModelPreview("3D 场景预览只支持 PMX、PMD 和 X".to_owned())),
    }
}

pub(crate) fn scene_preview_file(path: &Path) -> CoreResult<Vec<u8>> {
    let input = scene_view_input(path)?;
    let vertex_count = input.vertices.len();
    let group_count = input.material_ranges.len();
    let texture_paths = input.material_ranges.iter().map(|group| {
        input.materials.get(group.material_index).map(|material| material.texture_path.as_str()).unwrap_or("")
    }).collect::<Vec<_>>();
    let texture_bytes = texture_paths.iter().map(|path| 4usize.saturating_add(path.len())).sum::<usize>();
    let capacity = 24usize.checked_add(vertex_count.checked_mul(104).ok_or_else(|| CoreError::ModelPreview("场景网格过大".to_owned()))?)
        .and_then(|size| size.checked_add(input.indices.len().checked_mul(4)?))
        .and_then(|size| size.checked_add(group_count.checked_mul(24)?))
        .and_then(|size| size.checked_add(texture_bytes))
        .ok_or_else(|| CoreError::ModelPreview("场景预览大小溢出".to_owned()))?;
    if capacity > 256 * 1024 * 1024 { return Err(CoreError::ModelPreview("场景网格超过 256 MiB 预览上限".to_owned())); }
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(b"MMDV");
    for count in [3u32, vertex_count as u32, input.indices.len() as u32, group_count as u32, 0u32] {
        output.extend_from_slice(&count.to_le_bytes());
    }
    for (index, vertex) in input.vertices.iter().enumerate() {
        let uv = input.uvs.get(index).copied().unwrap_or([0.0; 2]);
        let payload = [vertex.position.x,vertex.position.y,vertex.position.z,
            vertex.normal.x,vertex.normal.y,vertex.normal.z,uv[0],uv[1],
            0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,
            0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0];
        for value in payload { output.extend_from_slice(&finite_or_zero(value).to_le_bytes()); }
    }
    for index in input.indices { output.extend_from_slice(&index.to_le_bytes()); }
    for group in input.material_ranges {
        output.extend_from_slice(&(group.start as u32).to_le_bytes());
        output.extend_from_slice(&(group.count as u32).to_le_bytes());
        let color = input.materials.get(group.material_index).map(|material| material.diffuse).unwrap_or([0.72,0.76,0.79,1.0]);
        for value in color { output.extend_from_slice(&finite_or_zero(value).to_le_bytes()); }
    }
    for path in texture_paths {
        output.extend_from_slice(&(path.len() as u32).to_le_bytes());
        output.extend_from_slice(path.as_bytes());
    }
    Ok(output)
}

pub(crate) fn scene_preview_texture_file(path: &Path, texture_path: &str) -> CoreResult<Option<(Vec<u8>, u8)>> {
    let input = scene_view_input(path)?;
    if !input.materials.iter().any(|material| material.texture_path == texture_path) {
        return Err(CoreError::ModelPreview("贴图未被该场景引用".to_owned()));
    }
    preview_texture_png(path, texture_path)
}

pub(crate) fn render_vpd_motion_file_with_progress(
    motion_path: &Path,
    preview_model_path: &Path,
    progress: &mut dyn FnMut(&str, f64) -> bool,
) -> CoreResult<GeneratedThumbnail> {
    if !motion_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vpd"))
    {
        return Err(CoreError::ThumbnailRender(
            "VPD 动作预览只支持 .vpd 姿势文件".to_owned(),
        ));
    }
    if !preview_model_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
    {
        return Err(CoreError::ThumbnailRender(
            "动作预览模型必须是 PMX 文件".to_owned(),
        ));
    }
    let parse_permit =
        thumbnail_concurrency::acquire(ThumbnailStage::Parse, || progress("Parsing", 0.05))?;
    if !progress("Parsing", 0.05) {
        return Err(CoreError::ThumbnailCancelled);
    }
    for path in [motion_path, preview_model_path] {
        if std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
            return Err(CoreError::ThumbnailRender(format!(
                "源文件超过 512 MiB 渲染上限：{}",
                path.display()
            )));
        }
    }
    let motion_bytes = std::fs::read(motion_path)?;
    let model_bytes = std::fs::read(preview_model_path)?;
    let pose = parse_vpd_pose(&motion_bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("VPD 解析失败：{error}")))?;
    let model = parse_pmx_model(&model_bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("PMX 解析失败：{error}")))?;
    if !progress("Parsing", 0.20) {
        return Err(CoreError::ThumbnailCancelled);
    }
    let mut input = render_input_from_pmx_with_pose(&model_bytes, &model, Some(&pose), None, None)?;
    input.framing_bounds = None;
    input.diagnostics.push("MotionPreview:VPD".to_owned());
    drop(parse_permit);
    render_motion_input_with_progress(preview_model_path, input, None, progress)
}

pub(crate) fn render_vmd_motion_file_with_progress(
    motion_path: &Path,
    preview_model_path: &Path,
    progress: &mut dyn FnMut(&str, f64) -> bool,
) -> CoreResult<GeneratedThumbnail> {
    if !motion_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vmd"))
    {
        return Err(CoreError::ThumbnailRender(
            "VMD 动作预览只支持 .vmd 动画文件".to_owned(),
        ));
    }
    if !preview_model_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pmx"))
    {
        return Err(CoreError::ThumbnailRender(
            "动作预览模型必须是 PMX 文件".to_owned(),
        ));
    }
    let parse_permit =
        thumbnail_concurrency::acquire(ThumbnailStage::Parse, || progress("Parsing", 0.05))?;
    if !progress("Parsing", 0.05) {
        return Err(CoreError::ThumbnailCancelled);
    }
    for path in [motion_path, preview_model_path] {
        if std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
            return Err(CoreError::ThumbnailRender(format!(
                "源文件超过 512 MiB 渲染上限：{}",
                path.display()
            )));
        }
    }
    let motion_bytes = std::fs::read(motion_path)?;
    let model_bytes = std::fs::read(preview_model_path)?;
    let vmd = mmd_anim_format::vmd::parse_vmd_shared_context(&motion_bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("VMD 解析失败：{error}")))?;
    let mut animation = vmd.import_result().clone();
    let model = parse_pmx_model(&model_bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("PMX 解析失败：{error}")))?;
    let imported = import_pmx_runtime(&model_bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("PMX 骨架解析失败：{error}")))?;
    if imported.model.bone_count() != model.skeleton.bones.len() {
        return Err(CoreError::ThumbnailRender(format!(
            "PMX 骨架数量不一致：解析器 {}，运行时 {}",
            model.skeleton.bones.len(),
            imported.model.bone_count()
        )));
    }
    let total_bone_frames = animation.bone_keyframes.len();
    let total_morph_frames = animation.morph_keyframes.len();
    let bone_is_mapped = |frame: &mmd_anim_format::vmd::VmdBoneKeyframeRaw| match &frame.bone_mode {
        mmd_anim_format::vmd::VmdBoneImportMode::ByName(_) => imported
            .bone_name_to_index
            .contains_key(&frame.bone_name_normalized),
        mmd_anim_format::vmd::VmdBoneImportMode::ByIndex(index) => {
            (*index as usize) < imported.model.bone_count()
        }
    };
    let unmapped_bone_frames = animation
        .bone_keyframes
        .iter()
        .filter(|frame| !bone_is_mapped(frame))
        .count();
    let unmapped_morph_frames = animation
        .morph_keyframes
        .iter()
        .filter(|(name, _, _)| {
            !imported
                .morph_name_to_index
                .contains_key(&mmd_anim_format::normalize_vmd_name(name))
        })
        .count();
    let preview_frame = animation
        .bone_keyframes
        .iter()
        .filter(|frame| bone_is_mapped(frame))
        .map(|frame| frame.frame)
        .chain(
            animation
                .morph_keyframes
                .iter()
                .filter(|(name, _, _)| {
                    imported
                        .morph_name_to_index
                        .contains_key(&mmd_anim_format::normalize_vmd_name(name))
                })
                .map(|(_, frame, _)| *frame),
        )
        .min()
        .unwrap_or(0);
    let camera_frames = &vmd.parsed_animation().camera_frames;
    let camera =
        mmd_anim_format::vmd::sample_vmd_camera_frames(camera_frames, preview_frame as f32);
    animation
        .bone_keyframes
        .retain(|frame| frame.frame <= preview_frame);
    animation
        .morph_keyframes
        .retain(|(_, frame, _)| *frame <= preview_frame);
    let clip = mmd_anim_format::vmd::build_clip_from_import(
        animation,
        &|name| imported.bone_name_to_index.get(name).copied(),
        &|name| imported.morph_name_to_index.get(name).copied(),
    );
    let sample = clip.sample_at(preview_frame as f32);
    if !progress("Parsing", 0.20) {
        return Err(CoreError::ThumbnailCancelled);
    }
    let mut input =
        render_input_from_pmx_with_pose(&model_bytes, &model, None, Some(&sample), camera)?;
    input.framing_bounds = None;
    input
        .diagnostics
        .push(format!("MotionPreview:VMD:{preview_frame}"));
    input.diagnostics.push(format!(
        "AppliedVmdBoneTracks:{}",
        sample.bone_samples().len()
    ));
    if total_bone_frames > 0 && unmapped_bone_frames > 0 {
        input
            .diagnostics
            .push(format!("UnmappedVmdBoneFrames:{unmapped_bone_frames}"));
    }
    if unmapped_morph_frames > 0 {
        input
            .diagnostics
            .push(format!("UnmappedVmdMorphFrames:{unmapped_morph_frames}"));
    }
    if total_bone_frames == 0 && total_morph_frames == 0 && camera_frames.is_empty() {
        input
            .diagnostics
            .push("VmdHasNoModelMotionChannels:rest-pose-preview".to_owned());
    } else if total_bone_frames == 0 && total_morph_frames == 0 {
        input
            .diagnostics
            .push("VmdHasNoModelMotionChannels:camera-preview".to_owned());
    }
    drop(parse_permit);
    render_motion_input_with_progress(preview_model_path, input, Some(preview_frame), progress)
}

fn render_motion_input_with_progress(
    preview_model_path: &Path,
    input: RenderInput,
    preview_frame: Option<u32>,
    progress: &mut dyn FnMut(&str, f64) -> bool,
) -> CoreResult<GeneratedThumbnail> {
    if !progress("Rendering", 0.40) {
        return Err(CoreError::ThumbnailCancelled);
    }
    let renderer = RENDERER
        .get_or_init(|| pollster::block_on(GpuRenderer::new()).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| CoreError::ThumbnailRender(error.clone()))?;
    let mut generated = renderer.render(preview_model_path, input, progress)?;
    generated.report.preview_settings_version =
        motion_preview_settings_version(preview_model_path)?;
    generated.report.preview_frame = preview_frame;
    Ok(generated)
}

fn render_input_from_pmx(bytes: &[u8], model: &PmxParsedModel) -> CoreResult<RenderInput> {
    render_input_from_pmx_with_pose(bytes, model, None, None, None)
}

fn render_input_from_pmx_with_pose(
    bytes: &[u8],
    model: &PmxParsedModel,
    pose: Option<&VpdParsedPose>,
    vmd_pose: Option<&ClipSample>,
    camera: Option<mmd_anim_format::vmd::VmdCameraState>,
) -> CoreResult<RenderInput> {
    let geometry = &model.geometry;
    let mut diagnostics = model
        .diagnostics
        .iter()
        .map(|diagnostic| format!("ParserDiagnostic:{}", diagnostic.code))
        .collect::<Vec<_>>();
    if let Some(pose) = pose {
        diagnostics.extend(
            pose.diagnostics
                .iter()
                .map(|diagnostic| format!("VpdDiagnostic:{}", diagnostic.code)),
        );
    }
    let mut uvs = geometry
        .uvs
        .chunks_exact(2)
        .map(|uv| [finite_or_zero(uv[0]), finite_or_zero(uv[1])])
        .collect::<Vec<_>>();
    let mut vertex_colors = geometry
        .additional_uvs
        .first()
        .filter(|values| values.len() == uvs.len() * 4)
        .map(|values| {
            values
                .chunks_exact(4)
                .map(|color| {
                    [
                        finite_or_zero(color[0]),
                        finite_or_zero(color[1]),
                        finite_or_zero(color[2]),
                        finite_or_zero(color[3]),
                    ]
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![[1.0; 4]; uvs.len()]);
    let material_ranges = geometry
        .material_groups
        .iter()
        .map(|group| MaterialRange {
            start: group.start,
            count: group.count,
            material_index: group.material_index,
        })
        .collect();
    let mut materials = model
        .materials
        .iter()
        .map(|material| RenderMaterial {
            name: material.name.clone(),
            texture_path: material.texture_path.clone(),
            sphere_texture_path: material.sphere_texture_path.clone(),
            toon_texture_path: material.toon_texture_path.clone(),
            // PMX shared toon indices 0..=9 correspond to toon01..toon10.
            shared_toon_index: material
                .shared_toon_index
                .map(|index| index.saturating_add(1)),
            sphere_mode: sphere_mode_code(&material.sphere_mode),
            diffuse: material.diffuse,
            ambient: [
                material.ambient[0],
                material.ambient[1],
                material.ambient[2],
                1.0,
            ],
            specular: [
                material.specular[0],
                material.specular[1],
                material.specular[2],
                material.specular_power,
            ],
            texture_factor: [1.0; 4],
            sphere_factor: [1.0; 4],
            toon_factor: [1.0; 4],
            toon_enabled: true,
            vertex_color_mode: if material.flags.vertex_color { 2 } else { 0 },
        })
        .collect::<Vec<_>>();
    let vertices = skin_vertices(
        bytes,
        model,
        pose,
        vmd_pose,
        &mut uvs,
        &mut vertex_colors,
        &mut materials,
        &mut diagnostics,
    )?;
    for material in &model.materials {
        if material.flags.vertex_color
            && geometry
                .additional_uvs
                .first()
                .is_none_or(|values| values.len() != uvs.len() * 4)
        {
            diagnostics.push(format!("MissingVertexColor:{}", material.name));
        }
        if material.flags.edge || material.flags.point_draw || material.flags.line_draw {
            diagnostics.push(format!("UnsupportedNonTriangleStyle:{}", material.name));
        }
    }
    Ok(RenderInput {
        vertices,
        uvs,
        indices: geometry.indices.clone(),
        material_ranges,
        materials,
        camera,
        scene_view: false,
        framing_bounds: character_skeleton_bounds(model),
        diagnostics,
    })
}

fn render_input_from_pmd(model: &PmdParsedModel) -> CoreResult<RenderInput> {
    let mut diagnostics = model
        .diagnostics
        .iter()
        .map(|diagnostic| format!("ParserDiagnostic:{}", diagnostic.code))
        .collect::<Vec<_>>();
    let mut invalid_normals = 0usize;
    let vertices = model
        .geometry
        .vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| {
            let position = Vec3::from_array(vertex.position);
            if !position.is_finite() {
                return Err(CoreError::ThumbnailRender(format!(
                    "PMD 顶点 {index} 包含无效坐标"
                )));
            }
            let normal = Vec3::from_array(vertex.normal);
            let normal = if normal.is_finite() && normal.length_squared() > 1.0e-12 {
                normal.normalize()
            } else {
                invalid_normals += 1;
                Vec3::Y
            };
            Ok(SkinnedVertex {
                position,
                normal,
                color: [1.0; 4],
            })
        })
        .collect::<CoreResult<Vec<_>>>()?;
    if invalid_normals > 0 {
        diagnostics.push(format!("InvalidNormalsReplaced:{invalid_normals}"));
    }

    let uvs = model
        .geometry
        .vertices
        .iter()
        .map(|vertex| [finite_or_zero(vertex.uv[0]), finite_or_zero(vertex.uv[1])])
        .collect::<Vec<_>>();
    let indices = model
        .geometry
        .indices
        .iter()
        .map(|index| u32::from(*index))
        .collect::<Vec<_>>();
    if indices.is_empty() || indices.len() % 3 != 0 {
        return Err(CoreError::ThumbnailRender(
            "PMD 网格不包含完整三角形".to_owned(),
        ));
    }
    if let Some(index) = indices
        .iter()
        .find(|index| **index as usize >= vertices.len())
    {
        return Err(CoreError::ThumbnailRender(format!(
            "PMD 网格索引 {index} 超出顶点范围 {}",
            vertices.len()
        )));
    }

    let mut material_ranges = Vec::with_capacity(model.materials.len());
    let mut materials = Vec::with_capacity(model.materials.len());
    let mut start = 0usize;
    for (material_index, material) in model.materials.iter().enumerate() {
        let count = usize::try_from(material.face_count)
            .ok()
            .and_then(|faces| faces.checked_mul(3))
            .ok_or_else(|| {
                CoreError::ThumbnailRender(format!(
                    "PMD 材质 {material_index} 的面数超出可处理范围"
                ))
            })?;
        let end = start.checked_add(count).ok_or_else(|| {
            CoreError::ThumbnailRender(format!("PMD 材质 {material_index} 的索引范围溢出"))
        })?;
        if end > indices.len() {
            return Err(CoreError::ThumbnailRender(format!(
                "PMD 材质 {material_index} 的索引范围超出网格"
            )));
        }
        material_ranges.push(MaterialRange {
            start,
            count,
            material_index,
        });
        start = end;

        let texture_parts = material
            .texture_name
            .split('*')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        let diffuse_texture = texture_parts.first().copied().filter(|path| {
            !Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("sph") || extension.eq_ignore_ascii_case("spa")
                })
        });
        if material.edge_enabled {
            diagnostics.push(format!("UnsupportedEdge:Material {material_index}"));
        }
        materials.push(RenderMaterial {
            name: format!("PMD material {material_index}"),
            texture_path: diffuse_texture.unwrap_or_default().to_owned(),
            sphere_texture_path: texture_parts
                .iter()
                .find(|path| {
                    Path::new(path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("sph")
                                || extension.eq_ignore_ascii_case("spa")
                        })
                })
                .copied()
                .unwrap_or_default()
                .to_owned(),
            toon_texture_path: model
                .toon_textures
                .get(usize::from(material.toon_index))
                .cloned()
                .unwrap_or_default(),
            // PMD toon indices 0..=9 select the ten standard toon maps.
            shared_toon_index: Some(material.toon_index.saturating_add(1)),
            sphere_mode: texture_parts
                .iter()
                .find(|path| {
                    Path::new(path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("sph")
                                || extension.eq_ignore_ascii_case("spa")
                        })
                })
                .and_then(|path| {
                    Path::new(path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                })
                .map(|extension| {
                    if extension.eq_ignore_ascii_case("sph") {
                        1
                    } else {
                        2
                    }
                })
                .unwrap_or(0),
            diffuse: material.diffuse.map(finite_or_zero),
            ambient: [
                finite_or_zero(material.ambient[0]),
                finite_or_zero(material.ambient[1]),
                finite_or_zero(material.ambient[2]),
                1.0,
            ],
            specular: [
                finite_or_zero(material.specular[0]),
                finite_or_zero(material.specular[1]),
                finite_or_zero(material.specular[2]),
                finite_or_zero(material.specular_power),
            ],
            texture_factor: [1.0; 4],
            sphere_factor: [1.0; 4],
            toon_factor: [1.0; 4],
            toon_enabled: true,
            vertex_color_mode: 0,
        });
    }
    if !materials.is_empty() && start != indices.len() {
        return Err(CoreError::ThumbnailRender(format!(
            "PMD 材质面数共覆盖 {start} 个索引，网格实际有 {} 个",
            indices.len()
        )));
    }
    if materials.is_empty() {
        diagnostics.push("MissingMaterial: using fallback material".to_owned());
    }
    Ok(RenderInput {
        vertices,
        uvs,
        indices,
        material_ranges,
        materials,
        camera: None,
        scene_view: false,
        framing_bounds: None,
        diagnostics,
    })
}

fn render_input_from_x(manifest: &AccessoryParsedManifest) -> CoreResult<RenderInput> {
    if manifest.mesh_summaries.is_empty() {
        return Err(CoreError::ThumbnailRender(
            "UnsupportedX: .x 场景不包含可用网格".to_owned(),
        ));
    }
    let mut vertices = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let mut material_ranges = Vec::<MaterialRange>::new();
    let mut diagnostics = manifest
        .diagnostics
        .iter()
        .map(|diagnostic| format!("ParserDiagnostic:{}", diagnostic.code))
        .collect::<Vec<_>>();
    let mut materials = manifest
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| RenderMaterial {
            name: material
                .name
                .clone()
                .unwrap_or_else(|| format!("Material {index}")),
            texture_path: material
                .texture_references
                .first()
                .cloned()
                .unwrap_or_default(),
            sphere_texture_path: String::new(),
            toon_texture_path: String::new(),
            shared_toon_index: None,
            sphere_mode: 0,
            diffuse: material
                .face_color
                .map(|color| color.map(finite_or_zero))
                .unwrap_or([0.72, 0.76, 0.79, 1.0]),
            ambient: material
                .emissive_color
                .map(|color| {
                    [
                        finite_or_zero(color[0]),
                        finite_or_zero(color[1]),
                        finite_or_zero(color[2]),
                        1.0,
                    ]
                })
                .unwrap_or([0.12, 0.12, 0.12, 1.0]),
            specular: [0.0, 0.0, 0.0, 1.0],
            texture_factor: [1.0; 4],
            sphere_factor: [1.0; 4],
            toon_factor: [1.0; 4],
            toon_enabled: false,
            vertex_color_mode: 1,
        })
        .collect::<Vec<_>>();
    if materials.is_empty() {
        diagnostics.push("MissingMaterial: using fallback material".to_owned());
    }

    for mesh in &manifest.mesh_summaries {
        let mut color_by_vertex =
            HashMap::<usize, [f32; 4]>::with_capacity(mesh.vertex_colors.len());
        for color in &mesh.vertex_colors {
            if let Some(vertex_color) = color_by_vertex.get_mut(&(color.vertex_index as usize)) {
                *vertex_color = color.color.map(finite_or_zero);
            } else {
                color_by_vertex
                    .insert(color.vertex_index as usize, color.color.map(finite_or_zero));
            }
        }
        if !color_by_vertex.is_empty() {
            let material_end = mesh
                .material_start_index
                .saturating_add(mesh.material_count)
                .min(materials.len());
            for material in materials
                .iter_mut()
                .take(material_end)
                .skip(mesh.material_start_index.min(material_end))
            {
                material.vertex_color_mode = 1;
            }
        }
        if mesh.texture_coordinates.len() != mesh.positions.len() {
            diagnostics.push("MissingTextureCoordinates:X mesh UVs are incomplete".to_owned());
        }
        for (face_index, face) in mesh.face_indices.iter().enumerate() {
            if face.len() < 3 {
                diagnostics.push(format!("IgnoredDegenerateFace:{face_index}"));
                continue;
            }
            let positions = face
                .iter()
                .map(|index| mesh.positions.get(*index as usize).copied())
                .collect::<Option<Vec<_>>>();
            let Some(positions) = positions else {
                return Err(CoreError::ThumbnailRender(format!(
                    "X 面 {face_index} 引用了范围外的顶点"
                )));
            };
            let material_index = mesh
                .material_indices
                .get(face_index)
                .copied()
                .map(|index| mesh.material_start_index + index as usize)
                .unwrap_or(0);
            let range_start = indices.len();
            for triangle_corner in 1..face.len() - 1 {
                let corners = [0, triangle_corner, triangle_corner + 1];
                let a = Vec3::from_array(positions[corners[0]]);
                let b = Vec3::from_array(positions[corners[1]]);
                let c = Vec3::from_array(positions[corners[2]]);
                let fallback_normal = (b - a).cross(c - a).normalize_or_zero();
                for corner in corners {
                    let position_index = face[corner] as usize;
                    let normal_index = mesh
                        .normal_face_indices
                        .get(face_index)
                        .and_then(|normal_face| normal_face.get(corner))
                        .copied()
                        .map(|index| index as usize)
                        .unwrap_or(position_index);
                    let normal = mesh
                        .normals
                        .get(normal_index)
                        .copied()
                        .map(Vec3::from_array)
                        .filter(|normal| normal.is_finite() && normal.length_squared() > 1.0e-12)
                        .unwrap_or(fallback_normal);
                    let position = Vec3::from_array(mesh.positions[position_index]);
                    if !position.is_finite() {
                        return Err(CoreError::ThumbnailRender(format!(
                            "X 顶点 {position_index} 包含无效坐标"
                        )));
                    }
                    vertices.push(SkinnedVertex {
                        position,
                        normal,
                        color: color_by_vertex
                            .get(&position_index)
                            .copied()
                            .unwrap_or([1.0; 4]),
                    });
                    uvs.push(
                        mesh.texture_coordinates
                            .get(position_index)
                            .copied()
                            .unwrap_or([0.0, 0.0]),
                    );
                    let index = u32::try_from(vertices.len() - 1).map_err(|_| {
                        CoreError::ThumbnailRender("X 网格顶点数超出 32 位范围".to_owned())
                    })?;
                    indices.push(index);
                }
            }
            let range_count = indices.len() - range_start;
            if range_count > 0 {
                if let Some(previous) = material_ranges.last_mut().filter(|previous| {
                    previous.material_index == material_index
                        && previous.start + previous.count == range_start
                }) {
                    previous.count += range_count;
                } else {
                    material_ranges.push(MaterialRange {
                        start: range_start,
                        count: range_count,
                        material_index,
                    });
                }
            }
        }
    }
    if indices.is_empty() {
        return Err(CoreError::ThumbnailRender(
            "X 场景没有可渲染的三角面".to_owned(),
        ));
    }
    Ok(RenderInput {
        vertices,
        uvs,
        indices,
        material_ranges,
        materials,
        camera: None,
        scene_view: false,
        framing_bounds: None,
        diagnostics,
    })
}

fn skin_vertices(
    bytes: &[u8],
    model: &PmxParsedModel,
    vpd_pose: Option<&VpdParsedPose>,
    vmd_pose: Option<&ClipSample>,
    uvs: &mut [[f32; 2]],
    vertex_colors: &mut [[f32; 4]],
    materials: &mut [RenderMaterial],
    diagnostics: &mut Vec<String>,
) -> CoreResult<Vec<SkinnedVertex>> {
    let geometry = &model.geometry;
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
        || geometry.qdef.enabled.len() != vertex_count
        || geometry.indices.len() % 3 != 0
    {
        return Err(CoreError::ThumbnailRender(
            "PMX 网格、骨骼权重或变形参数长度不一致".to_owned(),
        ));
    }
    if vertex_count == 0 || geometry.indices.is_empty() {
        return Err(CoreError::ThumbnailRender(
            "PMX 中没有可渲染的三角网格".to_owned(),
        ));
    }

    let imported = import_pmx_runtime(bytes)
        .map_err(|error| CoreError::ThumbnailRender(format!("PMX 骨架解析失败：{error}")))?;
    let mut runtime = RuntimeInstance::new(Arc::new(imported.model));
    runtime.evaluate_rest_pose();
    if let Some(vpd_pose) = vpd_pose {
        let mut bone_indices = HashMap::<&str, Vec<usize>>::new();
        for (index, bone) in model.skeleton.bones.iter().enumerate() {
            for name in [&bone.name, &bone.english_name] {
                if !name.trim().is_empty() {
                    let indices = bone_indices.entry(name.as_str()).or_default();
                    if !indices.contains(&index) {
                        indices.push(index);
                    }
                }
            }
        }
        for pose_bone in &vpd_pose.bones {
            let Some(indices) = bone_indices.get(pose_bone.name.as_str()) else {
                diagnostics.push(format!("UnmappedVpdBone:{}", pose_bone.name));
                continue;
            };
            if indices.len() != 1 {
                diagnostics.push(format!("AmbiguousVpdBone:{}", pose_bone.name));
                continue;
            }
            let translation = Vec3::from_array(pose_bone.translation);
            let rotation = Quat::from_xyzw(
                pose_bone.rotation[0],
                pose_bone.rotation[1],
                pose_bone.rotation[2],
                pose_bone.rotation[3],
            );
            if !translation.is_finite()
                || !rotation.is_finite()
                || rotation.length_squared() <= f32::EPSILON
            {
                diagnostics.push(format!("InvalidVpdBonePose:{}", pose_bone.name));
                continue;
            }
            let Ok(index) = u32::try_from(indices[0]) else {
                diagnostics.push(format!("InvalidVpdBoneIndex:{}", pose_bone.name));
                continue;
            };
            runtime
                .pose_mut()
                .set_local_position_offset(BoneIndex(index), translation.into());
            runtime
                .pose_mut()
                .set_local_rotation(BoneIndex(index), rotation.normalize());
        }
        runtime.evaluate_current_pose();
    }
    if let Some(vmd_pose) = vmd_pose {
        vmd_pose.apply_to_pose(runtime.pose_mut());
        let direct_weights = runtime.pose().morph_weights().to_vec();
        let (flip_additions, active_flip_offsets) =
            flip_morph_additions(&model.morphs, &direct_weights, diagnostics);
        for (index, addition) in flip_additions.into_iter().enumerate() {
            if addition != 0.0 {
                runtime.pose_mut().set_morph_weight(
                    MorphIndex(index as u32),
                    direct_weights.get(index).copied().unwrap_or_default() + addition,
                );
            }
        }
        if active_flip_offsets > 0 {
            diagnostics.push(format!("AppliedVmdFlipMorphOffsets:{active_flip_offsets}"));
        }
        runtime.expand_morphs();
        runtime.evaluate_current_pose();

        let current_bone_world = runtime.pose().world_matrices().to_vec();
        let rest_bone_world = (0..model.skeleton.bones.len())
            .map(|index| {
                runtime
                    .model()
                    .inverse_bind_matrix(BoneIndex(index as u32))
                    .inverse()
            })
            .collect::<Vec<_>>();
        if let Some(physics) = crate::thumbnail_physics::simulate_impulse_preview(
            &model.morphs,
            runtime.morph_weights(),
            &model.rigid_bodies,
            &model.joints,
            &current_bone_world,
            &rest_bone_world,
            diagnostics,
        ) {
            if physics.applied_offsets > 0 {
                let updated_bones =
                    runtime.apply_physics_world_matrices(&physics.bone_world_matrices);
                diagnostics.push(format!(
                    "AppliedVmdImpulseMorphs:{}:PhysicsSubsteps:{}:UpdatedBones:{}",
                    physics.applied_offsets, physics.simulated_substeps, updated_bones
                ));
            }
        }
    }
    let world_matrices = runtime.pose().world_matrices();
    if world_matrices.len() != model.skeleton.bones.len() {
        return Err(CoreError::ThumbnailRender(format!(
            "PMX 骨架数量不一致：解析器 {}，运行时 {}",
            model.skeleton.bones.len(),
            world_matrices.len()
        )));
    }
    let skin_matrices = (0..world_matrices.len())
        .map(|index| {
            world_matrices[index] * runtime.model().inverse_bind_matrix(BoneIndex(index as u32))
        })
        .collect::<Vec<_>>();

    let mut source_positions = geometry
        .positions
        .chunks_exact(3)
        .map(|position| {
            Vec3::new(
                finite_or_zero(position[0]),
                finite_or_zero(position[1]),
                finite_or_zero(position[2]),
            )
        })
        .collect::<Vec<_>>();
    let mut applied_vertex_morphs = 0usize;
    if vmd_pose.is_some() {
        let weights = runtime.morph_weights().to_vec();
        let mut applied_uv_morphs = 0usize;
        let mut applied_material_morphs = 0usize;
        let mut unsupported_additional_uv_morphs = 0usize;
        for (morph_index, morph) in model.morphs.iter().enumerate() {
            let Some(weight) = weights.get(morph_index).copied() else {
                diagnostics.push(format!("MissingVmdMorphWeight:{morph_index}"));
                continue;
            };
            if !weight.is_finite() || weight == 0.0 {
                continue;
            }
            let mut applied_vertex = false;
            for offset in &morph.vertex_offsets {
                let Some(position) = source_positions.get_mut(offset.vertex_index as usize) else {
                    diagnostics.push(format!(
                        "InvalidVertexMorphIndex:{morph_index}:{}",
                        offset.vertex_index
                    ));
                    continue;
                };
                let delta = Vec3::from_array(offset.position);
                if !delta.is_finite() {
                    diagnostics.push(format!("InvalidVertexMorphOffset:{morph_index}"));
                    continue;
                }
                *position += delta * weight;
                applied_vertex = true;
            }
            if applied_vertex {
                applied_vertex_morphs += 1;
            }

            let mut applied_uv = false;
            for offset in &morph.uv_offsets {
                let Some(uv) = uvs.get_mut(offset.vertex_index as usize) else {
                    diagnostics.push(format!(
                        "InvalidUvMorphIndex:{morph_index}:{}",
                        offset.vertex_index
                    ));
                    continue;
                };
                if offset.uv[..2].iter().any(|value| !value.is_finite()) {
                    diagnostics.push(format!("InvalidUvMorphOffset:{morph_index}"));
                    continue;
                }
                uv[0] += offset.uv[0] * weight;
                uv[1] += offset.uv[1] * weight;
                applied_uv = true;
            }
            if applied_uv {
                applied_uv_morphs += 1;
            }

            let mut applied_additional_uv = false;
            for offset in &morph.additional_uv_offsets {
                if offset.uv_index != 0 {
                    unsupported_additional_uv_morphs += 1;
                    continue;
                }
                let Some(color) = vertex_colors.get_mut(offset.vertex_index as usize) else {
                    diagnostics.push(format!(
                        "InvalidAdditionalUvMorphIndex:{morph_index}:{}",
                        offset.vertex_index
                    ));
                    continue;
                };
                if offset.uv.iter().any(|value| !value.is_finite()) {
                    diagnostics.push(format!("InvalidAdditionalUvMorphOffset:{morph_index}"));
                    continue;
                }
                for (channel, delta) in color.iter_mut().zip(offset.uv) {
                    *channel += delta * weight;
                }
                applied_additional_uv = true;
            }
            if applied_additional_uv {
                applied_uv_morphs += 1;
            }

            for offset in &morph.material_offsets {
                if apply_material_morph(offset, weight, materials, diagnostics, morph_index) {
                    applied_material_morphs += 1;
                }
            }
        }
        if unsupported_additional_uv_morphs > 0 {
            diagnostics.push(format!(
                "UnsupportedAdditionalUvMorphs:{unsupported_additional_uv_morphs}"
            ));
        }
        if applied_uv_morphs > 0 {
            diagnostics.push(format!("AppliedVmdUvMorphTracks:{applied_uv_morphs}"));
        }
        if applied_material_morphs > 0 {
            diagnostics.push(format!(
                "AppliedVmdMaterialMorphOffsets:{applied_material_morphs}"
            ));
        }
    }
    if applied_vertex_morphs > 0 {
        diagnostics.push(format!(
            "AppliedVmdVertexMorphTracks:{applied_vertex_morphs}"
        ));
    }
    let mut source_normals = (0..vertex_count)
        .map(|index| {
            let base = index * 3;
            Vec3::new(
                finite_or_zero(geometry.normals[base]),
                finite_or_zero(geometry.normals[base + 1]),
                finite_or_zero(geometry.normals[base + 2]),
            )
            .normalize_or_zero()
        })
        .collect::<Vec<_>>();
    if applied_vertex_morphs > 0 {
        source_normals =
            recalculated_vertex_normals(&source_positions, &geometry.indices, &source_normals);
    }

    let mut vertices = Vec::with_capacity(vertex_count);
    for index in 0..vertex_count {
        let position = source_positions[index];
        let normal = source_normals[index];
        let weights = &geometry.skin_weights[index * 4..index * 4 + 4];
        let bone_indices = &geometry.skin_indices[index * 4..index * 4 + 4];
        let mode = geometry.sdef.skinning_modes[index].as_str();
        let skinned = match mode {
            "bdef1" => skin_linear(position, normal, weights, bone_indices, &skin_matrices, 1),
            "bdef2" => skin_linear(position, normal, weights, bone_indices, &skin_matrices, 2),
            "bdef4" => skin_linear(position, normal, weights, bone_indices, &skin_matrices, 4),
            "sdef" => skin_sdef(
                position,
                normal,
                index,
                weights,
                bone_indices,
                &skin_matrices,
                geometry,
            ),
            "qdef" => skin_qdef(position, normal, weights, bone_indices, &skin_matrices),
            other => {
                return Err(CoreError::ThumbnailRender(format!(
                    "不支持的 PMX 蒙皮类型：{other}"
                )));
            }
        }
        .ok_or_else(|| {
            CoreError::ThumbnailRender(format!(
                "顶点 {index} 的骨骼引用或权重无效，无法生成可靠缩略图"
            ))
        })?;
        let mut skinned = skinned;
        skinned.color = vertex_colors.get(index).copied().unwrap_or([1.0; 4]);
        vertices.push(skinned);
    }
    Ok(vertices)
}

fn flip_morph_additions(
    morphs: &[mmd_anim_format::pmx::PmxParsedMorph],
    direct_weights: &[f32],
    diagnostics: &mut Vec<String>,
) -> (Vec<f32>, usize) {
    let mut additions = vec![0.0; morphs.len()];
    let mut activated = HashSet::<(usize, usize)>::new();
    let mut invalid_offsets = 0usize;
    let maximum_passes = morphs.len().saturating_add(1);
    for _ in 0..maximum_passes {
        let combined = (0..morphs.len())
            .map(|index| direct_weights.get(index).copied().unwrap_or_default() + additions[index])
            .collect::<Vec<_>>();
        let expanded = expand_group_morph_weights(morphs, &combined);
        let mut changed = false;
        for (morph_index, morph) in morphs.iter().enumerate() {
            if morph.kind != "flip" {
                continue;
            }
            let weight = expanded.get(morph_index).copied().unwrap_or_default();
            if !weight.is_finite() || weight <= 0.0 {
                continue;
            }
            for (offset_index, offset) in morph.flip_offsets.iter().enumerate() {
                let target = usize::try_from(offset.morph_index).ok();
                let Some(target) = target.filter(|target| *target < morphs.len()) else {
                    invalid_offsets += 1;
                    continue;
                };
                if !offset.weight.is_finite() {
                    invalid_offsets += 1;
                    continue;
                }
                if weight >= offset.weight && activated.insert((morph_index, offset_index)) {
                    additions[target] = additions[target].max((1.0 - expanded[target]).max(0.0));
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    if invalid_offsets > 0 {
        diagnostics.push(format!("InvalidVmdFlipMorphOffsets:{invalid_offsets}"));
    }
    (additions, activated.len())
}

fn expand_group_morph_weights(
    morphs: &[mmd_anim_format::pmx::PmxParsedMorph],
    direct_weights: &[f32],
) -> Vec<f32> {
    let mut expanded = direct_weights.to_vec();
    let mut stack = Vec::<(usize, f32, usize)>::new();
    for (morph_index, weight) in direct_weights
        .iter()
        .copied()
        .take(morphs.len())
        .enumerate()
    {
        if weight == 0.0 || !weight.is_finite() {
            continue;
        }
        if morphs[morph_index].kind == "group" {
            stack.push((morph_index, weight, 0));
        }
        while let Some((parent, parent_weight, offset_index)) = stack.pop() {
            let Some(offset) = morphs[parent].group_offsets.get(offset_index) else {
                continue;
            };
            stack.push((parent, parent_weight, offset_index + 1));
            let Ok(child) = usize::try_from(offset.morph_index) else {
                continue;
            };
            let Some(child_weight) = expanded.get_mut(child) else {
                continue;
            };
            let contribution = parent_weight * offset.weight;
            *child_weight += contribution;
            if contribution != 0.0 && contribution.is_finite() && morphs[child].kind == "group" {
                stack.push((child, contribution, 0));
            }
        }
    }
    expanded
}

fn apply_material_morph(
    offset: &mmd_anim_format::pmx::PmxParsedMaterialMorphOffset,
    weight: f32,
    materials: &mut [RenderMaterial],
    diagnostics: &mut Vec<String>,
    morph_index: usize,
) -> bool {
    let operation = match offset.operation.as_str() {
        "add" => 0,
        "multiply" => 1,
        _ => {
            diagnostics.push(format!("UnsupportedMaterialMorphOperation:{morph_index}"));
            return false;
        }
    };
    let material_indices = if offset.material_index == -1 {
        (0..materials.len()).collect::<Vec<_>>()
    } else if let Ok(index) = usize::try_from(offset.material_index) {
        if index < materials.len() {
            vec![index]
        } else {
            diagnostics.push(format!(
                "InvalidMaterialMorphIndex:{morph_index}:{}",
                offset.material_index
            ));
            return false;
        }
    } else {
        diagnostics.push(format!(
            "InvalidMaterialMorphIndex:{morph_index}:{}",
            offset.material_index
        ));
        return false;
    };
    if [
        offset.diffuse.as_slice(),
        offset.specular.as_slice(),
        offset.ambient.as_slice(),
        offset.texture_factor.as_slice(),
        offset.sphere_texture_factor.as_slice(),
        offset.toon_texture_factor.as_slice(),
        std::slice::from_ref(&offset.specular_power),
    ]
    .iter()
    .any(|values| values.iter().any(|value| !value.is_finite()))
    {
        diagnostics.push(format!("InvalidMaterialMorphOffset:{morph_index}"));
        return false;
    }
    for index in &material_indices {
        let material = &mut materials[*index];
        apply_material_factor(&mut material.diffuse, &offset.diffuse, weight, operation);
        apply_material_factor(
            &mut material.specular[..3],
            &offset.specular,
            weight,
            operation,
        );
        apply_material_scalar(
            &mut material.specular[3],
            offset.specular_power,
            weight,
            operation,
        );
        apply_material_factor(
            &mut material.ambient[..3],
            &offset.ambient,
            weight,
            operation,
        );
        apply_material_factor(
            &mut material.texture_factor,
            &offset.texture_factor,
            weight,
            operation,
        );
        apply_material_factor(
            &mut material.sphere_factor,
            &offset.sphere_texture_factor,
            weight,
            operation,
        );
        apply_material_factor(
            &mut material.toon_factor,
            &offset.toon_texture_factor,
            weight,
            operation,
        );
    }
    !material_indices.is_empty()
}

fn apply_material_factor(current: &mut [f32], target: &[f32], weight: f32, operation: u8) {
    for (current, target) in current.iter_mut().zip(target) {
        apply_material_scalar(current, *target, weight, operation);
    }
}

fn apply_material_scalar(current: &mut f32, target: f32, weight: f32, operation: u8) {
    if operation == 0 {
        *current += target * weight;
    } else {
        *current *= 1.0 + (target - 1.0) * weight;
    }
}

fn sphere_mode_code(mode: &str) -> u32 {
    match mode {
        "multiply" => 1,
        "add" => 2,
        "subTexture" => 3,
        _ => 0,
    }
}

fn recalculated_vertex_normals(
    positions: &[Vec3],
    indices: &[u32],
    fallback: &[Vec3],
) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let (Some(position_a), Some(position_b), Some(position_c)) =
            (positions.get(a), positions.get(b), positions.get(c))
        else {
            continue;
        };
        let face_normal = (*position_b - *position_a).cross(*position_c - *position_a);
        if !face_normal.is_finite() || face_normal.length_squared() <= f32::EPSILON {
            continue;
        }
        for index in [a, b, c] {
            normals[index] += face_normal;
        }
    }

    normals
        .into_iter()
        .enumerate()
        .map(|(index, normal)| {
            let normal = normal.normalize_or_zero();
            if normal == Vec3::ZERO {
                fallback.get(index).copied().unwrap_or(Vec3::ZERO)
            } else {
                normal
            }
        })
        .collect()
}

fn skin_linear(
    position: Vec3,
    normal: Vec3,
    weights: &[f32],
    bone_indices: &[u32],
    matrices: &[Mat4],
    influence_count: usize,
) -> Option<SkinnedVertex> {
    let mut total_weight = 0.0f32;
    for weight in weights.iter().take(influence_count) {
        if weight.is_finite() && *weight > 0.0 {
            total_weight += *weight;
        }
    }
    if total_weight <= f32::EPSILON {
        return Some(SkinnedVertex {
            position,
            normal: normal.normalize_or_zero(),
            color: [1.0; 4],
        });
    }

    let mut output_position = Vec3::ZERO;
    let mut output_normal = Vec3::ZERO;
    for slot in 0..influence_count {
        let raw_weight = weights[slot];
        if !raw_weight.is_finite() || raw_weight <= 0.0 {
            continue;
        }
        let matrix = *matrices.get(bone_indices[slot] as usize)?;
        let weight = raw_weight / total_weight;
        output_position += matrix.transform_point3(position) * weight;
        output_normal += matrix.transform_vector3(normal) * weight;
    }
    Some(SkinnedVertex {
        position: output_position,
        normal: output_normal.normalize_or_zero(),
        color: [1.0; 4],
    })
}

fn skin_sdef(
    position: Vec3,
    normal: Vec3,
    vertex_index: usize,
    weights: &[f32],
    bone_indices: &[u32],
    matrices: &[Mat4],
    geometry: &mmd_anim_format::pmx::PmxParsedGeometry,
) -> Option<SkinnedVertex> {
    let mut weight0 = weights[0].clamp(0.0, 1.0);
    let mut weight1 = weights[1].clamp(0.0, 1.0);
    let total = weight0 + weight1;
    if total <= f32::EPSILON || !total.is_finite() {
        return Some(SkinnedVertex {
            position,
            normal: normal.normalize_or_zero(),
            color: [1.0; 4],
        });
    }
    weight0 /= total;
    weight1 /= total;
    let matrix0 = *matrices.get(bone_indices[0] as usize)?;
    let matrix1 = *matrices.get(bone_indices[1] as usize)?;
    let base = vertex_index * 3;
    let center = Vec3::new(
        geometry.sdef.c[base],
        geometry.sdef.c[base + 1],
        geometry.sdef.c[base + 2],
    );
    let r0 = Vec3::new(
        geometry.sdef.r0[base],
        geometry.sdef.r0[base + 1],
        geometry.sdef.r0[base + 2],
    );
    let r1 = Vec3::new(
        geometry.sdef.r1[base],
        geometry.sdef.r1[base + 1],
        geometry.sdef.r1[base + 2],
    );
    let weighted_r = r0 * weight0 + r1 * weight1;
    let moved_r0 = matrix0.transform_point3(center + r0 - weighted_r);
    let moved_r1 = matrix1.transform_point3(center + r1 - weighted_r);
    let moved_c0 = matrix0.transform_point3(center);
    let moved_c1 = matrix1.transform_point3(center);
    let delta = (moved_r0 + moved_c0 - center) * weight0 + (moved_r1 + moved_c1 - center) * weight1;
    let translation = (center + delta) * 0.5;
    let rotation0 = matrix0.to_scale_rotation_translation().1.normalize();
    let rotation1 = matrix1.to_scale_rotation_translation().1.normalize();
    let rotation = rotation1.slerp(rotation0, weight0).normalize();
    Some(SkinnedVertex {
        position: rotation * (position - center) + translation,
        normal: (rotation * normal).normalize_or_zero(),
        color: [1.0; 4],
    })
}

fn skin_qdef(
    position: Vec3,
    normal: Vec3,
    weights: &[f32],
    bone_indices: &[u32],
    matrices: &[Mat4],
) -> Option<SkinnedVertex> {
    let mut reference: Option<Quat> = None;
    let mut active = Vec::with_capacity(4);
    let mut total_weight = 0.0f32;
    for slot in 0..4 {
        let weight = weights[slot];
        if !weight.is_finite() || weight <= 0.0 {
            continue;
        }
        let matrix = *matrices.get(bone_indices[slot] as usize)?;
        let (scale, mut rotation, translation) = matrix.to_scale_rotation_translation();
        if !scale.is_finite() || !rotation.is_finite() || !translation.is_finite() {
            return None;
        }
        rotation = rotation.normalize();
        if let Some(reference_rotation) = reference {
            if reference_rotation.dot(rotation) < 0.0 {
                rotation = -rotation;
            }
        } else {
            reference = Some(rotation);
        }
        total_weight += weight;
        active.push((rotation, translation, weight));
    }
    if total_weight <= f32::EPSILON || !total_weight.is_finite() {
        return Some(SkinnedVertex {
            position,
            normal: normal.normalize_or_zero(),
            color: [1.0; 4],
        });
    }
    let mut real = Quat::from_xyzw(0.0, 0.0, 0.0, 0.0);
    let mut dual = Quat::from_xyzw(0.0, 0.0, 0.0, 0.0);
    for (rotation, translation, weight) in active {
        let normalized_weight = weight / total_weight;
        let translation_quaternion =
            Quat::from_xyzw(0.0, translation.x, translation.y, translation.z);
        let dual_part = (translation_quaternion * rotation) * 0.5;
        real += rotation * normalized_weight;
        dual += dual_part * normalized_weight;
    }
    let length = real.length();
    if length <= f32::EPSILON || !length.is_finite() {
        return None;
    }
    real = real / length;
    dual = dual / length;
    dual = dual - real * real.dot(dual);
    let translation = dual * real.conjugate() * 2.0;
    let rotation = real.normalize();
    Some(SkinnedVertex {
        position: rotation * position + Vec3::new(translation.x, translation.y, translation.z),
        normal: (rotation * normal).normalize_or_zero(),
        color: [1.0; 4],
    })
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

impl GpuRenderer {
    async fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..Default::default()
            })
            .await
            .map_err(|error| format!("无法创建离屏渲染适配器：{error}"))?;
        let adapter_name = {
            let info = adapter.get_info();
            format!("{} ({:?}/{:?})", info.name, info.backend, info.device_type)
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("MMDbridge thumbnail renderer"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("无法创建设备：{error}"))?;

        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("MMDbridge material layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<MaterialUniform>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("MMDbridge thumbnail pipeline layout"),
            bind_group_layouts: &[Some(&material_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("MMDbridge thumbnail shader"),
            source: wgpu::ShaderSource::Wgsl(THUMBNAIL_SHADER.into()),
        });
        let vertex_attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4];
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &vertex_attributes,
        })];
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let opaque_pipeline =
            create_pipeline(&device, &pipeline_layout, &shader, &buffers, &targets, true);
        let transparent_pipeline = create_pipeline(
            &device,
            &pipeline_layout,
            &shader,
            &buffers,
            &targets,
            false,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("MMDbridge material sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let toon_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("MMDbridge toon sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Ok(Self {
            device,
            queue,
            adapter_name,
            opaque_pipeline,
            transparent_pipeline,
            material_layout,
            sampler,
            toon_sampler,
        })
    }

    fn render(
        &self,
        source_path: &Path,
        input: RenderInput,
        progress: &mut dyn FnMut(&str, f64) -> bool,
    ) -> CoreResult<GeneratedThumbnail> {
        let render_permit =
            thumbnail_concurrency::acquire(ThumbnailStage::Render, || progress("Rendering", 0.40))?;
        let RenderInput {
            vertices: source_vertices,
            uvs,
            indices,
            material_ranges,
            materials,
            camera,
            scene_view,
            framing_bounds,
            mut diagnostics,
        } = input;
        if source_vertices.len() != uvs.len() || indices.is_empty() {
            return Err(CoreError::ThumbnailRender(
                "网格顶点、UV 或索引数据不完整".to_owned(),
            ));
        }
        let (minimum, maximum) = bounds(&source_vertices)?;
        let mesh_half_extent = ((maximum.x - minimum.x)
            .max(maximum.y - minimum.y) * 0.5).max(0.001) / 0.92;
        let bone_half_extent = framing_bounds.map(|(bone_min, bone_max)| {
            ((bone_max.x - bone_min.x).max(bone_max.y - bone_min.y) * 0.5).max(0.001) / 0.82
        });
        let (center, half_extent) = if bone_half_extent.is_some_and(|extent| extent >= mesh_half_extent) {
            let (bone_min, bone_max) = framing_bounds.expect("bone extent requires bounds");
            diagnostics.push("CharacterBoneFraming".to_owned());
            ((bone_min + bone_max) * 0.5, bone_half_extent.unwrap())
        } else {
            if framing_bounds.is_some() { diagnostics.push("CharacterMeshFraming".to_owned()); }
            ((minimum + maximum) * 0.5, mesh_half_extent)
        };
        let depth_range = (maximum.z - minimum.z).max(0.001);
        let camera_view_projection = if scene_view {
            diagnostics.push("SceneWideCamera:165cm:90deg".to_owned());
            Some(scene_camera_view_projection(&source_vertices))
        } else {
            camera.and_then(|camera| {
                vmd_camera_view_projection(camera, &source_vertices, &mut diagnostics)
            })
        };
        if camera.is_some() && camera_view_projection.is_none() {
            diagnostics.push("InvalidVmdCamera:usingAutoFraming".to_owned());
        }
        if let Some(camera) = camera.filter(|_| camera_view_projection.is_some()) {
            diagnostics.push(format!(
                "VmdCameraProjection:{}:Fov:{}",
                if camera.perspective {
                    "perspective"
                } else {
                    "orthographic"
                },
                camera.fov
            ));
        }
        let mut gpu_vertices = Vec::with_capacity(source_vertices.len());
        for (index, vertex) in source_vertices.iter().enumerate() {
            let depth = (vertex.position.z - minimum.z) / depth_range;
            let position = if let Some(view_projection) = camera_view_projection {
                (view_projection * vertex.position.extend(1.0)).to_array()
            } else {
                [
                    (center.x - vertex.position.x) / half_extent,
                    (vertex.position.y - center.y) / half_extent,
                    0.002 + depth.clamp(0.0, 1.0) * 0.996,
                    1.0,
                ]
            };
            gpu_vertices.push(GpuVertex {
                position,
                normal: vertex.normal.to_array(),
                uv: [finite_or_zero(uvs[index][0]), finite_or_zero(uvs[index][1])],
                color: vertex.color.map(finite_or_zero),
            });
        }

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("MMDbridge thumbnail vertex buffer"),
                contents: bytemuck::cast_slice(&gpu_vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("MMDbridge thumbnail index buffer"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        for index in &indices {
            if *index as usize >= source_vertices.len() {
                return Err(CoreError::ThumbnailRender(format!(
                    "网格索引 {index} 超出顶点范围 {}",
                    source_vertices.len()
                )));
            }
        }
        let (texture_resources, material_bind_groups, texture_count, texture_alpha) =
            self.create_material_bind_groups(source_path, &materials, &mut diagnostics)?;
        let groups = build_draw_groups(
            &indices,
            &material_ranges,
            &gpu_vertices,
            &materials,
            &texture_alpha,
            &mut diagnostics,
        )?;
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MMDbridge 1024 thumbnail target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("MMDbridge thumbnail depth target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        let unpadded_bytes_per_row = WIDTH * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(alignment) * alignment;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("MMDbridge thumbnail readback"),
            size: padded_bytes_per_row as u64 * HEIGHT as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("MMDbridge thumbnail render encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("MMDbridge thumbnail pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.055,
                            g: 0.075,
                            b: 0.09,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            for group in groups.iter().filter(|group| !group.transparent) {
                pass.set_pipeline(&self.opaque_pipeline);
                let bind_group = material_bind_groups
                    .get(group.material_index)
                    .unwrap_or(&material_bind_groups[0]);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw_indexed(group.start..group.end, 0, 0..1);
            }
            let mut transparent = groups
                .iter()
                .filter(|group| group.transparent)
                .collect::<Vec<_>>();
            transparent.sort_by(|left, right| right.depth.total_cmp(&left.depth));
            pass.set_pipeline(&self.transparent_pipeline);
            for group in transparent {
                let bind_group = material_bind_groups
                    .get(group.material_index)
                    .unwrap_or(&material_bind_groups[0]);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw_indexed(group.start..group.end, 0, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (send, receive) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(60)),
            })
            .map_err(|error| CoreError::ThumbnailRender(format!("GPU 等待失败：{error}")))?;
        receive
            .recv()
            .map_err(|error| CoreError::ThumbnailRender(format!("读取 GPU 结果失败：{error}")))?
            .map_err(|error| CoreError::ThumbnailRender(format!("映射 GPU 缓冲区失败：{error}")))?;
        let mapped = slice.get_mapped_range().map_err(|error| {
            CoreError::ThumbnailRender(format!("读取 GPU 缓冲区映射失败：{error}"))
        })?;
        let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for row in mapped.chunks_exact(padded_bytes_per_row as usize) {
            rgba.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        readback.unmap();
        drop((texture_resources, target, depth));

        drop(render_permit);
        let _encode_permit =
            thumbnail_concurrency::acquire(ThumbnailStage::Encode, || progress("Encoding", 0.90))?;
        if !progress("Encoding", 0.90) {
            return Err(CoreError::ThumbnailCancelled);
        }
        let preview_webp = webp::Encoder::from_rgba(&rgba, WIDTH, HEIGHT)
            .encode_simple(false, QUALITY)
            .map_err(|error| CoreError::ThumbnailRender(format!("WebP Q50 编码失败：{error:?}")))?
            .to_vec();
        if !progress("Encoding", 0.98) {
            return Err(CoreError::ThumbnailCancelled);
        }
        Ok(GeneratedThumbnail {
            preview_webp,
            report: ThumbnailRenderReport {
                renderer_version: RENDERER_VERSION.to_owned(),
                preview_settings_version: if scene_view {
                    SCENE_PREVIEW_SETTINGS_VERSION.to_owned()
                } else {
                    PREVIEW_SETTINGS_VERSION.to_owned()
                },
                adapter: self.adapter_name.clone(),
                front_axis: if scene_view {
                    "scene center -Z, eye 165cm (+Y up)".to_owned()
                } else if camera_view_projection.is_some() {
                    "VMD camera track (+Y up)".to_owned()
                } else {
                    "-Z (+Y up)".to_owned()
                },
                width: WIDTH,
                height: HEIGHT,
                format: "webp".to_owned(),
                quality: QUALITY as u8,
                preview_frame: None,
                triangle_count: indices.len() / 3,
                material_count: materials.len(),
                texture_count,
                vertex_count: source_vertices.len(),
                diagnostics,
            },
        })
    }

    fn create_material_bind_groups(
        &self,
        source_path: &Path,
        materials: &[RenderMaterial],
        diagnostics: &mut Vec<String>,
    ) -> CoreResult<(Vec<TextureResource>, Vec<wgpu::BindGroup>, usize, Vec<TextureAlpha>)> {
        let fallback = TextureData {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255],
            alpha: TextureAlpha::default(),
        };
        let (fallback_texture, fallback_view) = self.upload_texture(&fallback, "white fallback");
        let mut textures = vec![TextureResource {
            _texture: fallback_texture,
            view: fallback_view,
        }];
        let mut texture_resource_alpha = vec![TextureAlpha::default()];
        let mut texture_cache = HashMap::<String, usize>::new();
        let mut bind_groups = Vec::with_capacity(materials.len().max(1));
        let mut texture_alpha = Vec::with_capacity(materials.len().max(1));
        let source_dir = source_path.parent().unwrap_or_else(|| Path::new("."));
        let mut texture_count = 0usize;

        if materials.is_empty() {
            let fallback_material = RenderMaterial {
                name: "fallback".to_owned(),
                texture_path: String::new(),
                sphere_texture_path: String::new(),
                toon_texture_path: String::new(),
                shared_toon_index: None,
                sphere_mode: 0,
                diffuse: [0.72, 0.76, 0.79, 1.0],
                ambient: [0.12, 0.12, 0.12, 1.0],
                specular: [0.0, 0.0, 0.0, 1.0],
                texture_factor: [1.0; 4],
                sphere_factor: [1.0; 4],
                toon_factor: [1.0; 4],
                toon_enabled: false,
                vertex_color_mode: 0,
            };
            bind_groups.push(self.create_material_bind_group(
                &textures[0].view,
                &textures[0].view,
                &textures[0].view,
                material_uniform(&fallback_material),
            ));
            texture_alpha.push(TextureAlpha::default());
        }
        for material in materials {
            let (diffuse_index, alpha) = self.resolve_texture_index(
                source_dir,
                &material.texture_path,
                &material.name,
                "Texture",
                &mut textures,
                &mut texture_resource_alpha,
                &mut texture_cache,
                &mut texture_count,
                diagnostics,
            );
            let (sphere_index, _) = self.resolve_texture_index(
                source_dir,
                &material.sphere_texture_path,
                &material.name,
                "SphereTexture",
                &mut textures,
                &mut texture_resource_alpha,
                &mut texture_cache,
                &mut texture_count,
                diagnostics,
            );
            let (mut toon_index, _) = self.resolve_texture_index(
                source_dir,
                &material.toon_texture_path,
                &material.name,
                "ToonTexture",
                &mut textures,
                &mut texture_resource_alpha,
                &mut texture_cache,
                &mut texture_count,
                diagnostics,
            );
            if material.toon_enabled && toon_index == 0 {
                let toon_id = material.shared_toon_index.unwrap_or(0);
                let key = format!("mmd-shared-toon-{toon_id}");
                toon_index = if let Some(index) = texture_cache.get(&key).copied() {
                    index
                } else {
                    match builtin_toon_texture(toon_id) {
                        Ok(Some(data)) => {
                            let (texture, view) =
                                self.upload_texture(&data, "MMD shared toon texture");
                            let index = textures.len();
                            textures.push(TextureResource {
                                _texture: texture,
                                view,
                            });
                            texture_resource_alpha.push(data.alpha);
                            texture_cache.insert(key, index);
                            index
                        }
                        Ok(None) => {
                            diagnostics.push(format!(
                                "InvalidSharedToonIndex:{}:{toon_id}",
                                material.name
                            ));
                            0
                        }
                        Err(error) => {
                            diagnostics
                                .push(format!("BuiltinToonDecodeFailed:{}:{error}", material.name));
                            0
                        }
                    }
                };
            }
            let mut uniform = material_uniform(material);
            if sphere_index == 0 {
                uniform.flags[0] = 0;
            }
            uniform.flags[3] = u32::from(alpha.masked);
            bind_groups.push(self.create_material_bind_group(
                &textures[diffuse_index].view,
                &textures[sphere_index].view,
                &textures[toon_index].view,
                uniform,
            ));
            texture_alpha.push(alpha);
        }
        Ok((textures, bind_groups, texture_count, texture_alpha))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_texture_index(
        &self,
        source_dir: &Path,
        texture_path: &str,
        material_name: &str,
        diagnostic_prefix: &str,
        textures: &mut Vec<TextureResource>,
        texture_resource_alpha: &mut Vec<TextureAlpha>,
        texture_cache: &mut HashMap<String, usize>,
        texture_count: &mut usize,
        diagnostics: &mut Vec<String>,
    ) -> (usize, TextureAlpha) {
        if texture_path.trim().is_empty() {
            return (0, TextureAlpha::default());
        }
        let found = texture_candidates(source_dir, texture_path)
            .into_iter()
            .find(|candidate| candidate.is_file());
        let Some(path) = found else {
            diagnostics.push(format!(
                "Missing{diagnostic_prefix}:{material_name}:{texture_path}"
            ));
            return (0, TextureAlpha::default());
        };
        let key = path.to_string_lossy().to_lowercase();
        if let Some(index) = texture_cache.get(&key).copied() {
            return (
                index,
                texture_resource_alpha.get(index).copied().unwrap_or_default(),
            );
        }
        let max_dimension =
            MAX_TEXTURE_DIMENSION.min(self.device.limits().max_texture_dimension_2d);
        match load_texture(&path, max_dimension) {
            Ok((data, resized)) => {
                if resized {
                    diagnostics.push(format!("TextureResized:{material_name}:{}", path.display()));
                }
                let alpha = data.alpha;
                let (texture, view) = self.upload_texture(&data, material_name);
                let index = textures.len();
                textures.push(TextureResource {
                    _texture: texture,
                    view,
                });
                texture_resource_alpha.push(alpha);
                texture_cache.insert(key, index);
                *texture_count += 1;
                (index, alpha)
            }
            Err(error) => {
                diagnostics.push(format!(
                    "{diagnostic_prefix}LoadFailed:{material_name}:{texture_path}:{error}"
                ));
                (0, TextureAlpha::default())
            }
        }
    }

    fn upload_texture(
        &self,
        data: &TextureData,
        label: &str,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: data.width,
                height: data.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(data.width * 4),
                rows_per_image: Some(data.height),
            },
            wgpu::Extent3d {
                width: data.width,
                height: data.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn create_material_bind_group(
        &self,
        diffuse_view: &wgpu::TextureView,
        sphere_view: &wgpu::TextureView,
        toon_view: &wgpu::TextureView,
        uniform: MaterialUniform,
    ) -> wgpu::BindGroup {
        let uniform = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("MMDbridge material uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("MMDbridge material bind group"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(diffuse_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(sphere_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(toon_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.toon_sampler),
                },
            ],
        })
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    targets: &[Option<wgpu::ColorTargetState>],
    depth_write_enabled: bool,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if depth_write_enabled {
            "MMDbridge opaque thumbnail pipeline"
        } else {
            "MMDbridge transparent thumbnail pipeline"
        }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers,
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(depth_write_enabled),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn build_draw_groups(
    indices: &[u32],
    material_ranges: &[MaterialRange],
    vertices: &[GpuVertex],
    materials: &[RenderMaterial],
    texture_alpha: &[TextureAlpha],
    diagnostics: &mut Vec<String>,
) -> CoreResult<Vec<DrawGroup>> {
    let mut groups = Vec::new();
    for group in material_ranges.iter().filter(|group| group.count > 0) {
        let end = group
            .start
            .checked_add(group.count)
            .filter(|end| *end <= indices.len())
            .ok_or_else(|| CoreError::ThumbnailRender("材质索引范围超出网格".to_owned()))?;
        if group.count % 3 != 0 {
            return Err(CoreError::ThumbnailRender(
                "材质组索引数量不是三角形的整数倍".to_owned(),
            ));
        }
        let material_index = if group.material_index < materials.len() {
            group.material_index
        } else {
            diagnostics.push(format!("MissingMaterial:{}", group.material_index));
            0
        };
        let depth = indices[group.start..end]
            .iter()
            .filter_map(|index| vertices.get(*index as usize))
            .map(|vertex| vertex.position[2])
            .filter(|z| z.is_finite())
            .sum::<f32>()
            / group.count.max(1) as f32;
        let diffuse_alpha = materials
            .get(material_index)
            .map_or(1.0, |material| material.diffuse[3]);
        let texture_transparent = texture_alpha.get(material_index).is_some_and(|alpha| alpha.blended);
        let vertex_transparent = materials
            .get(material_index)
            .is_some_and(|material| material.vertex_color_mode != 0)
            && indices[group.start..end].iter().any(|index| {
                vertices
                    .get(*index as usize)
                    .is_some_and(|vertex| vertex.color[3] < 0.999)
            });
        groups.push(DrawGroup {
            start: u32::try_from(group.start)
                .map_err(|_| CoreError::ThumbnailRender("材质索引起点超出 32 位范围".to_owned()))?,
            end: u32::try_from(end)
                .map_err(|_| CoreError::ThumbnailRender("材质索引终点超出 32 位范围".to_owned()))?,
            material_index,
            depth,
            transparent: diffuse_alpha < 0.999 || texture_transparent || vertex_transparent,
        });
    }
    if groups.is_empty() && !indices.is_empty() {
        diagnostics.push("MissingMaterialGroups: using fallback material".to_owned());
        groups.push(DrawGroup {
            start: 0,
            end: u32::try_from(indices.len())
                .map_err(|_| CoreError::ThumbnailRender("PMX 索引数量超出 32 位范围".to_owned()))?,
            material_index: 0,
            depth: 0.0,
            transparent: false,
        });
    }
    Ok(groups)
}

fn bounds(vertices: &[SkinnedVertex]) -> CoreResult<(Vec3, Vec3)> {
    let mut minimum = Vec3::splat(f32::INFINITY);
    let mut maximum = Vec3::splat(f32::NEG_INFINITY);
    for vertex in vertices {
        if !vertex.position.is_finite() {
            return Err(CoreError::ThumbnailRender(
                "骨骼蒙皮后包含非有限顶点坐标".to_owned(),
            ));
        }
        minimum = minimum.min(vertex.position);
        maximum = maximum.max(vertex.position);
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return Err(CoreError::ThumbnailRender("模型边界框无效".to_owned()));
    }
    Ok((minimum, maximum))
}

fn character_skeleton_bounds(model: &PmxParsedModel) -> Option<(Vec3, Vec3)> {
    let bones = &model.skeleton.bones;
    if bones.len() < 12 {
        return None;
    }
    let has_head = bones.iter().any(|bone| {
        bone.name.contains('頭') || bone.english_name.to_ascii_lowercase().contains("head")
    });
    let has_leg = bones.iter().any(|bone| {
        bone.name.contains('足') || bone.english_name.to_ascii_lowercase().contains("leg")
    });
    if !has_head || !has_leg {
        return None;
    }
    let mut axes = [Vec::new(), Vec::new(), Vec::new()];
    for bone in bones {
        for (axis, value) in axes.iter_mut().zip(bone.position) {
            if value.is_finite() { axis.push(value); }
        }
    }
    if axes.iter().any(|axis| axis.len() < 12) {
        return None;
    }
    let mut minimum = [0.0; 3];
    let mut maximum = [0.0; 3];
    for (index, axis) in axes.iter_mut().enumerate() {
        axis.sort_by(f32::total_cmp);
        let trim = axis.len() / 100;
        minimum[index] = axis[trim];
        maximum[index] = axis[axis.len() - 1 - trim];
    }
    let (minimum, maximum) = (Vec3::from_array(minimum), Vec3::from_array(maximum));
    (maximum.y - minimum.y >= 1.0 && maximum.x - minimum.x >= 0.1)
        .then_some((minimum, maximum))
}

fn scene_camera_view_projection(vertices: &[SkinnedVertex]) -> Mat4 {
    // MMD's commonly used scale is approximately 8 cm per world unit.
    let eye = Vec3::new(0.0, 165.0 / 8.0, 0.0);
    let far = vertices.iter()
        .map(|vertex| vertex.position.distance(eye))
        .filter(|distance| distance.is_finite())
        .fold(250.0f32, f32::max)
        .mul_add(1.25, 0.0)
        .min(100_000.0);
    Mat4::perspective_rh(90.0f32.to_radians(), WIDTH as f32 / HEIGHT as f32, 0.1, far)
        * Mat4::look_at_rh(eye, eye - Vec3::Z, Vec3::Y)
}

fn vmd_camera_view_projection(
    camera: mmd_anim_format::vmd::VmdCameraState,
    vertices: &[SkinnedVertex],
    diagnostics: &mut Vec<String>,
) -> Option<Mat4> {
    let target = Vec3::from_array(camera.position);
    if !target.is_finite()
        || !camera.distance.is_finite()
        || !camera.rotation.iter().all(|value| value.is_finite())
        || !camera.fov.is_finite()
        || camera.distance.abs() <= 1.0e-4
    {
        return None;
    }

    let rotation = Quat::from_euler(
        glam::EulerRot::XYZ,
        -camera.rotation[0],
        -camera.rotation[1],
        -camera.rotation[2],
    );
    if !rotation.is_finite() || rotation.length_squared() <= f32::EPSILON {
        return None;
    }
    let rotation = rotation.normalize();
    let eye = target + rotation * Vec3::new(0.0, 0.0, -camera.distance);
    let direction = (target - eye).normalize_or_zero();
    if !eye.is_finite() || direction == Vec3::ZERO {
        return None;
    }

    let mut up = rotation * Vec3::Y;
    if up.cross(direction).length_squared() <= 1.0e-6 {
        up = if direction.cross(Vec3::Y).length_squared() > 1.0e-6 {
            Vec3::Y
        } else {
            Vec3::Z
        };
        diagnostics.push("VmdCameraRollFallback:worldUp".to_owned());
    }
    let view = Mat4::look_at_rh(eye, target, up);
    if !view.is_finite() {
        return None;
    }

    let positive_depths = vertices
        .iter()
        .map(|vertex| -view.transform_point3(vertex.position).z)
        .filter(|depth| depth.is_finite() && *depth > 1.0e-3)
        .collect::<Vec<_>>();
    let Some(closest) = positive_depths.iter().copied().reduce(f32::min) else {
        diagnostics.push("VmdCameraHasNoVisibleVertices".to_owned());
        return None;
    };
    let farthest = positive_depths.iter().copied().reduce(f32::max)?;
    let near = (closest * 0.5).max(1.0e-3);
    let far = (farthest * 1.5).max(near + 1.0);
    let fov = camera.fov.clamp(1.0, 179.0);
    if fov != camera.fov {
        diagnostics.push(format!("VmdCameraFovClamped:{}:{fov}", camera.fov));
    }

    let projection = if camera.perspective {
        Mat4::perspective_rh(fov.to_radians(), WIDTH as f32 / HEIGHT as f32, near, far)
    } else {
        let half_height = camera.distance.abs() * (fov.to_radians() * 0.5).tan();
        if !half_height.is_finite() || half_height <= 1.0e-4 {
            return None;
        }
        let half_width = half_height * WIDTH as f32 / HEIGHT as f32;
        Mat4::orthographic_rh(
            -half_width,
            half_width,
            -half_height,
            half_height,
            near,
            far,
        )
    };
    let view_projection = projection * view;
    view_projection.is_finite().then_some(view_projection)
}

fn texture_candidates(source_dir: &Path, texture_path: &str) -> Vec<PathBuf> {
    let raw = PathBuf::from(texture_path);
    if raw.is_absolute() {
        vec![raw]
    } else {
        vec![source_dir.join(raw)]
    }
}

fn material_uniform(material: &RenderMaterial) -> MaterialUniform {
    MaterialUniform {
        diffuse: material.diffuse,
        ambient: material.ambient,
        specular: material.specular,
        texture_factor: material.texture_factor,
        sphere_factor: material.sphere_factor,
        toon_factor: material.toon_factor,
        flags: [
            material.sphere_mode,
            u32::from(material.toon_enabled),
            material.vertex_color_mode,
            0,
        ],
    }
}

fn texture_alpha_mode(rgba: &[u8]) -> TextureAlpha {
    let mut cutout_pixels = 0usize;
    let mut fractional_pixels = 0usize;
    for alpha in rgba.chunks_exact(4).map(|pixel| pixel[3]) {
        cutout_pixels += usize::from(alpha == 0);
        fractional_pixels += usize::from(alpha > 0 && alpha < 255);
    }
    let masked = cutout_pixels > 0 && fractional_pixels * 4 <= rgba.len() / 4;
    TextureAlpha {
        masked,
        blended: fractional_pixels > 0 && !masked,
    }
}

fn builtin_toon_texture(index: u8) -> Result<Option<TextureData>, String> {
    // Built-in toon PNGs from Three.js r160 MMDLoader.js (MIT, © Three.js Authors).
    let png = match index {
        0 | 7..=10 => include_bytes!("../assets/mmd-toon/toon00.png").as_slice(),
        1 => include_bytes!("../assets/mmd-toon/toon01.png").as_slice(),
        2 => include_bytes!("../assets/mmd-toon/toon02.png").as_slice(),
        3 => include_bytes!("../assets/mmd-toon/toon03.png").as_slice(),
        4 => include_bytes!("../assets/mmd-toon/toon04.png").as_slice(),
        5 => include_bytes!("../assets/mmd-toon/toon05.png").as_slice(),
        6 => include_bytes!("../assets/mmd-toon/toon06.png").as_slice(),
        _ => return Ok(None),
    };
    let rgba = image::load_from_memory(png)
        .map_err(|error| format!("embedded Toon PNG is invalid: {error}"))?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let alpha = texture_alpha_mode(rgba.as_raw());
    Ok(Some(TextureData {
        width,
        height,
        rgba: rgba.into_raw(),
        alpha,
    }))
}

fn load_texture(path: &Path, max_dimension: u32) -> Result<(TextureData, bool), String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("无法读取纹理信息：{error}"))?;
    if metadata.len() > MAX_TEXTURE_SOURCE_BYTES {
        return Err("纹理文件超过 128 MiB 解码上限".to_owned());
    }
    let mut reader = ImageReader::open(path).map_err(|error| format!("无法打开纹理：{error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_TEXTURE_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_TEXTURE_DECODE_DIMENSION);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("不支持或损坏的纹理：{error}"))?
        .to_rgba8();
    let (original_width, original_height) = decoded.dimensions();
    if original_width == 0 || original_height == 0 {
        return Err("纹理尺寸为零".to_owned());
    }
    let maximum = original_width.max(original_height);
    let max_dimension = max_dimension.max(1);
    let resized = maximum > max_dimension;
    let rgba = if resized {
        let scale = max_dimension as f32 / maximum as f32;
        image::imageops::resize(
            &decoded,
            ((original_width as f32 * scale).round() as u32).max(1),
            ((original_height as f32 * scale).round() as u32).max(1),
            FilterType::Triangle,
        )
    } else {
        decoded
    };
    let alpha = texture_alpha_mode(rgba.as_raw());
    Ok((
        TextureData {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            alpha,
        },
        resized,
    ))
}

pub(crate) fn preview_texture_png(model_path: &Path, texture_path: &str) -> CoreResult<Option<(Vec<u8>, u8)>> {
    let source_dir = model_path.parent().unwrap_or_else(|| Path::new("."));
    let Some(path) = texture_candidates(source_dir, texture_path)
        .into_iter().find(|candidate| candidate.is_file()) else { return Ok(None); };
    let (data, _) = load_texture(&path, 2048).map_err(CoreError::ModelPreview)?;
    // Some PMX textures are effectively opaque but contain a faint alpha channel.
    // Keeping those opaque avoids sorting entire overlapping material groups as glass.
    let nearly_opaque = data.rgba.chunks_exact(4).all(|pixel| pixel[3] >= 224);
    let alpha_mode = if nearly_opaque { 0 } else if data.alpha.masked { 1 } else if data.alpha.blended { 2 } else { 0 };
    let image = image::RgbaImage::from_raw(data.width, data.height, data.rgba)
        .ok_or_else(|| CoreError::ModelPreview("decoded texture size is invalid".to_owned()))?;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| CoreError::ModelPreview(format!("texture PNG encode failed: {error}")))?;
    Ok(Some((png.into_inner(), alpha_mode)))
}

const THUMBNAIL_SHADER: &str = r#"
struct Material {
    diffuse: vec4<f32>,
    ambient: vec4<f32>,
    specular: vec4<f32>,
    texture_factor: vec4<f32>,
    sphere_factor: vec4<f32>,
    toon_factor: vec4<f32>,
    flags: vec4<u32>,
};

@group(0) @binding(0) var diffuse_texture: texture_2d<f32>;
@group(0) @binding(1) var diffuse_sampler: sampler;
@group(0) @binding(2) var sphere_texture: texture_2d<f32>;
@group(0) @binding(3) var toon_texture: texture_2d<f32>;
@group(0) @binding(4) var<uniform> material: Material;
@group(0) @binding(5) var toon_sampler: sampler;

struct VertexInput {
    @location(0) position: vec4<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = input.position;
    output.normal = input.normal;
    output.uv = input.uv;
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    var normal = normalize(input.normal);
    if (!front_facing) {
        normal = -normal;
    }
    let light_direction = normalize(vec3<f32>(0.28, 0.45, 0.85));
    let diffuse_light = max(dot(normal, light_direction), 0.0);
    let texel = textureSample(diffuse_texture, diffuse_sampler, input.uv);
    if (material.flags.w == 1u && texel.a < 0.5) {
        discard;
    }
    var base_color = material.diffuse.rgb;
    var vertex_alpha = 1.0;
    if (material.flags.z == 1u) {
        base_color *= input.color.rgb;
        vertex_alpha = input.color.a;
    } else if (material.flags.z == 2u) {
        base_color = input.color.rgb;
        vertex_alpha = input.color.a;
    }

    var lighting = vec3<f32>(0.3 + 0.7 * diffuse_light);
    if (material.flags.y != 0u) {
        let toon_uv = vec2<f32>(0.5, 1.0 - diffuse_light);
        lighting = textureSample(toon_texture, toon_sampler, toon_uv).rgb * material.toon_factor.rgb;
    }
    var color = texel.rgb * material.texture_factor.rgb * base_color * lighting;
    color += material.ambient.rgb * 0.22;

    if (material.flags.x != 0u) {
        let view_direction = vec3<f32>(0.0, 0.0, 1.0);
        let x_axis = normalize(vec3<f32>(view_direction.z, 0.0, -view_direction.x));
        let y_axis = cross(view_direction, x_axis);
        let sphere_uv = clamp(vec2<f32>(dot(x_axis, normal), dot(y_axis, normal)) * 0.495 + 0.5, vec2<f32>(0.0), vec2<f32>(1.0));
        var mapped_uv = sphere_uv;
        if (material.flags.x == 3u) {
            mapped_uv = input.uv;
        }
        let sphere_texel = textureSample(sphere_texture, diffuse_sampler, mapped_uv);
        let sphere_color = sphere_texel.rgb * material.sphere_factor.rgb;
        if (material.flags.x == 1u || material.flags.x == 3u) {
            color *= mix(vec3<f32>(1.0), sphere_color, sphere_texel.a * material.sphere_factor.a);
        } else if (material.flags.x == 2u) {
            color += sphere_color * sphere_texel.a * material.sphere_factor.a;
        }
    }

    let half_vector = normalize(light_direction + vec3<f32>(0.0, 0.0, 1.0));
    let specular_strength = pow(max(dot(normal, half_vector), 0.0), max(material.specular.a, 1.0));
    color += material.specular.rgb * specular_strength * diffuse_light;
    // A restrained view-facing matcap fill keeps dark diffuse materials legible in small cards.
    let matcap_normal = normalize(normal + vec3<f32>(0.0, 0.0, 0.35));
    let matcap_light = pow(max(dot(matcap_normal, normalize(vec3<f32>(-0.32, 0.55, 0.78))), 0.0), 3.0);
    color += vec3<f32>(0.075, 0.09, 0.105) * matcap_light;
    return vec4<f32>(color, texel.a * material.texture_factor.a * material.diffuse.a * vertex_alpha);
}
"#;
