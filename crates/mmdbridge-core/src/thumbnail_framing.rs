use std::collections::HashMap;

use glam::{DVec3, Vec3};
use mmd_anim_format::{PmdParsedModel, PmxParsedModel};

use super::{CoreError, CoreResult, MaterialRange, RenderMaterial, SkinnedVertex};

pub(super) struct SubjectFrame {
    pub minimum: DVec3,
    pub maximum: DVec3,
    pub indices: Vec<u32>,
    pub ranges: Vec<MaterialRange>,
}

pub(super) fn project(position: Vec3) -> DVec3 {
    let p = position.as_dvec3();
    // Camera-space arithmetic stays in f64, including finite extreme source coordinates.
    let (sin, cos) = (-5.0_f64.to_radians()).sin_cos();
    DVec3::new(p.x, cos * p.y - sin * p.z, sin * p.y + cos * p.z)
}

pub(super) fn indexed_bounds(vertices: &[SkinnedVertex], indices: &[u32]) -> CoreResult<(DVec3, DVec3)> {
    let mut minimum = DVec3::splat(f64::INFINITY);
    let mut maximum = DVec3::splat(f64::NEG_INFINITY);
    for &index in indices {
        let vertex = vertices.get(index as usize)
            .ok_or_else(|| CoreError::ThumbnailRender("网格索引超出顶点范围".to_owned()))?;
        let position = project(vertex.position);
        if !position.is_finite() {
            return Err(CoreError::ThumbnailRender("骨骼蒙皮后包含非有限顶点坐标".to_owned()));
        }
        minimum = minimum.min(position);
        maximum = maximum.max(position);
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return Err(CoreError::ThumbnailRender("模型边界框无效".to_owned()));
    }
    Ok((minimum, maximum))
}

fn body_bone(name: &str, english_name: &str) -> bool {
    let japanese = name.trim().trim_start_matches(['左', '右']);
    if ["頭", "首", "上半身", "上半身2", "上半身3", "下半身", "腰", "肩", "腕", "ひじ", "手首", "足", "ひざ", "足首"]
        .contains(&japanese) { return true; }
    [name, english_name].iter().any(|name| {
        let normalized = name.rsplit(':').next().unwrap_or(name).to_ascii_lowercase()
            .replace([' ', '_', '.', '-'], "");
        let normalized = normalized.trim_end_matches(|c: char| c.is_ascii_digit());
        let names = ["head", "neck", "hips", "pelvis", "spine", "chest", "upperbody", "lowerbody", "shoulder",
            "upperarm", "arm", "forearm", "elbow", "wrist", "hand", "thigh", "upperleg", "lowerleg",
            "leg", "knee", "ankle", "foot"];
        names.contains(&normalized) || ["left", "right", "l", "r"].iter()
            .any(|prefix| normalized.strip_prefix(*prefix).is_some_and(|name| names.contains(&name)))
    })
}

pub(super) fn pmx_body_weights(model: &PmxParsedModel) -> Vec<f32> {
    let bones = model.skeleton.bones.iter().map(|bone| body_bone(&bone.name, &bone.english_name))
        .collect::<Vec<_>>();
    model.geometry.skin_indices.chunks_exact(4).zip(model.geometry.skin_weights.chunks_exact(4))
        .map(|(indices, weights)| indices.iter().zip(weights).filter_map(|(&index, &weight)| {
            (weight.is_finite() && weight > 0.0 && bones.get(index as usize) == Some(&true))
                .then_some(weight)
        }).sum::<f32>().clamp(0.0, 1.0)).collect()
}

pub(super) fn pmd_body_weights(model: &PmdParsedModel) -> Vec<f32> {
    let bones = model.skeleton.bones.iter().map(|bone| body_bone(&bone.name, &bone.english_name))
        .collect::<Vec<_>>();
    model.geometry.vertices.iter().map(|vertex| {
        let weight = f32::from(vertex.bone_weight.min(100)) / 100.0;
        let [a, b] = vertex.bone_indices;
        (if bones.get(a as usize) == Some(&true) { weight } else { 0.0 })
            + (if bones.get(b as usize) == Some(&true) { 1.0 - weight } else { 0.0 })
    }).collect()
}

fn checkpoint(progress: &mut dyn FnMut(&str, f64) -> bool, index: usize) -> CoreResult<()> {
    if index % 8192 == 0 && !progress("Rendering", 0.41) {
        return Err(CoreError::ThumbnailCancelled);
    }
    Ok(())
}

fn robust_bounds(points: &[DVec3]) -> (DVec3, DVec3) {
    let mut minimum = DVec3::ZERO;
    let mut maximum = DVec3::ZERO;
    // Quantiles estimate a seed scale only. The final crop uses complete retained surfaces.
    let trim = if points.len() >= 12 { (points.len() / 100).max(1) } else { 0 };
    for axis in 0..3 {
        let mut values = points.iter().map(|point| point[axis]).collect::<Vec<_>>();
        values.select_nth_unstable_by(trim, f64::total_cmp);
        minimum[axis] = values[trim];
        let end = values.len() - 1 - trim;
        values.select_nth_unstable_by(end, f64::total_cmp);
        maximum[axis] = values[end];
    }
    (minimum, maximum)
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() { return 0.0; }
    let middle = values.len() / 2;
    values.select_nth_unstable_by(middle, f64::total_cmp);
    values[middle]
}

struct Components {
    parent: Vec<u32>,
    size: Vec<u32>,
}

impl Components {
    fn new(count: usize) -> Self {
        Self { parent: (0..count as u32).collect(), size: vec![1; count] }
    }
    fn root(&mut self, mut vertex: u32) -> u32 {
        while self.parent[vertex as usize] != vertex {
            let parent = self.parent[vertex as usize];
            self.parent[vertex as usize] = self.parent[parent as usize];
            vertex = parent;
        }
        vertex
    }
    fn join(&mut self, a: u32, b: u32) {
        let (mut a, mut b) = (self.root(a), self.root(b));
        if a == b { return; }
        if self.size[a as usize] < self.size[b as usize] { std::mem::swap(&mut a, &mut b); }
        self.parent[b as usize] = a;
        self.size[a as usize] += self.size[b as usize];
    }
}

struct Surface {
    minimum: DVec3,
    maximum: DVec3,
    score: f64,
    body_support: f64,
}

fn area(points: [DVec3; 3]) -> f64 {
    (points[1] - points[0]).cross(points[2] - points[0]).length() * 0.5
}

fn gap(a_min: DVec3, a_max: DVec3, b_min: DVec3, b_max: DVec3) -> f64 {
    (a_min - b_max).max(b_min - a_max).max(DVec3::ZERO).length()
}

pub(super) fn subject_frame(
    vertices: &[SkinnedVertex], indices: &[u32], ranges: &[MaterialRange], materials: &[RenderMaterial],
    body_weights: &[f32], diagnostics: &mut Vec<String>, progress: &mut dyn FnMut(&str, f64) -> bool,
) -> CoreResult<SubjectFrame> {
    let full = indexed_bounds(vertices, indices)?;
    if indices.len() % 3 != 0 {
        return Err(CoreError::ThumbnailRender("网格索引数量不是三角形的整数倍".to_owned()));
    }
    let fallback = || SubjectFrame { minimum: full.0, maximum: full.1, indices: indices.to_vec(),
        ranges: ranges.iter().map(|range| MaterialRange { start: range.start, count: range.count,
            material_index: range.material_index }).collect() };
    let positions = vertices.iter().map(|vertex| project(vertex.position)).collect::<Vec<_>>();
    let mut visible = vec![true; indices.len() / 3];
    let mut covered = 0;
    for range in ranges {
        let end = range.start.checked_add(range.count).filter(|&end| end <= indices.len())
            .ok_or_else(|| CoreError::ThumbnailRender("材质索引范围超出网格".to_owned()))?;
        if range.start % 3 != 0 || range.count % 3 != 0 {
            return Err(CoreError::ThumbnailRender("材质组索引数量不是三角形的整数倍".to_owned()));
        }
        if range.start != covered {
            return Err(CoreError::ThumbnailRender("材质组未连续覆盖网格索引".to_owned()));
        }
        covered = end;
        if let Some(material) = materials.get(range.material_index) {
            let alpha = material.diffuse[3] * material.texture_factor[3];
            for triangle in range.start / 3..end / 3 {
                checkpoint(progress, triangle)?;
                let vertex_alpha = material.vertex_color_mode == 0 || indices[triangle * 3..triangle * 3 + 3]
                    .iter().any(|&index| vertices[index as usize].color[3] > 0.001);
                visible[triangle] = alpha > 0.001 && vertex_alpha;
            }
        }
    }
    if !ranges.is_empty() && covered != indices.len() {
        return Err(CoreError::ThumbnailRender("材质组未完整覆盖网格索引".to_owned()));
    }
    let invisible_count = visible.iter().filter(|&&value| !value).count();
    let mut referenced = vec![false; vertices.len()];
    for (triangle, face) in indices.chunks_exact(3).enumerate() {
        checkpoint(progress, triangle)?;
        if visible[triangle] { for &index in face { referenced[index as usize] = true; } }
    }
    let points = positions.iter().zip(&referenced).filter_map(|(&point, &used)| used.then_some(point))
        .collect::<Vec<_>>();
    if points.is_empty() {
        diagnostics.push("SubjectFraming:NoVisibleSurface:FullMeshFallback".to_owned());
        return Ok(fallback());
    }
    let (seed_min, seed_max) = robust_bounds(&points);
    drop(points);
    let seed_span = (seed_max - seed_min).max_element();
    let stride = (indices.len() / 3).div_ceil(8192).max(1);
    let mut edge_samples = Vec::new();
    let mut area_samples = Vec::new();
    for triangle in (0..visible.len()).step_by(stride) {
        if !visible[triangle] { continue; }
        let face = &indices[triangle * 3..triangle * 3 + 3];
        let p = [positions[face[0] as usize], positions[face[1] as usize], positions[face[2] as usize]];
        for length in [p[0].distance(p[1]), p[1].distance(p[2]), p[2].distance(p[0])] {
            if length > 0.0 { edge_samples.push(length); }
        }
        let area = area(p);
        if area > 0.0 { area_samples.push(area); }
    }
    let edge_median = median(&mut edge_samples);
    let area_cap = median(&mut area_samples) * 128.0;
    if edge_median <= 0.0 || area_cap <= 0.0 {
        diagnostics.push("SubjectFraming:DegenerateSurface:FullMeshFallback".to_owned());
        return Ok(fallback());
    }
    // Split only grossly stretched faces; normal long hair, skirts and wings remain connected.
    let reference_span = seed_span.max(edge_median * 32.0).min(edge_median * 4096.0);
    let edge_limit = reference_span * 6.0;
    let mut components = Components::new(vertices.len());
    let mut reliable = vec![false; visible.len()];
    for (triangle, face) in indices.chunks_exact(3).enumerate() {
        checkpoint(progress, triangle)?;
        if !visible[triangle] { continue; }
        let p = [positions[face[0] as usize], positions[face[1] as usize], positions[face[2] as usize]];
        if [p[0].distance(p[1]), p[1].distance(p[2]), p[2].distance(p[0])]
            .iter().any(|&length| length > edge_limit) { continue; }
        reliable[triangle] = true;
        components.join(face[0], face[1]); components.join(face[1], face[2]);
    }
    let mut surfaces = HashMap::<u32, Surface>::new();
    for (triangle, face) in indices.chunks_exact(3).enumerate() {
        checkpoint(progress, triangle)?;
        if !reliable[triangle] { continue; }
        let p = [positions[face[0] as usize], positions[face[1] as usize], positions[face[2] as usize]];
        let body = face.iter().map(|&index| body_weights.get(index as usize).copied().unwrap_or(0.0))
            .filter(|value| value.is_finite()).map(f64::from).sum::<f64>() / 3.0;
        let support = area(p).min(area_cap);
        let surface = surfaces.entry(components.root(face[0])).or_insert(Surface {
            minimum: DVec3::splat(f64::INFINITY), maximum: DVec3::splat(f64::NEG_INFINITY),
            score: 0.0, body_support: 0.0,
        });
        for point in p { surface.minimum = surface.minimum.min(point); surface.maximum = surface.maximum.max(point); }
        surface.score += support * (1.0 + 8.0 * body);
        surface.body_support += support * body;
    }
    // Resolve equal support by vertex order, independent of HashMap iteration order.
    let Some((&anchor, surface)) = surfaces.iter().filter(|(_, surface)| surface.score > 0.0)
        .max_by(|(a, left), (b, right)| left.score.total_cmp(&right.score).then_with(|| b.cmp(a))) else {
        diagnostics.push("SubjectFraming:NoReliableSurface:FullMeshFallback".to_owned());
        return Ok(fallback());
    };
    let has_body_support = surface.body_support > 0.0;
    referenced.fill(false);
    for (triangle, face) in indices.chunks_exact(3).enumerate() {
        checkpoint(progress, triangle)?;
        if reliable[triangle] && components.root(face[0]) == anchor {
            for &index in face { referenced[index as usize] = true; }
        }
    }
    let anchor_points = positions.iter().zip(&referenced).filter_map(|(&point, &used)| used.then_some(point))
        .collect::<Vec<_>>();
    let (anchor_min, anchor_max) = robust_bounds(&anchor_points);
    drop(anchor_points);
    let span = (anchor_max - anchor_min).max_element().max(edge_median * 2.0);
    let vicinity = span * 0.75;
    let safety_min = anchor_min - DVec3::splat(span * 6.0);
    let safety_max = anchor_max + DVec3::splat(span * 6.0);
    let mut keep = std::collections::HashSet::from([anchor]);
    let mut cluster_min = anchor_min;
    let mut cluster_max = anchor_max;
    // Include adjoining disconnected parts in batches, preserving UV seams and limbs.
    // A fixed safety envelope prevents a chain of remote fragments from walking the crop away.
    for _ in 0..16 {
        let mut additions = Vec::new();
        for (index, (&root, surface)) in surfaces.iter().enumerate() {
            checkpoint(progress, index)?;
            if keep.contains(&root) || gap(cluster_min, cluster_max, surface.minimum, surface.maximum) > vicinity {
                continue;
            }
            if surface.minimum.cmpgt(safety_max).any() || surface.maximum.cmplt(safety_min).any() { continue; }
            additions.push(root);
        }
        if additions.is_empty() { break; }
        for root in additions {
            let surface = &surfaces[&root]; keep.insert(root);
            cluster_min = cluster_min.min(surface.minimum.max(safety_min));
            cluster_max = cluster_max.max(surface.maximum.min(safety_max));
        }
    }
    let excluded_components = surfaces.len() - keep.len();
    let mut retained = vec![false; reliable.len()];
    let mut minimum = DVec3::splat(f64::INFINITY);
    let mut maximum = DVec3::splat(f64::NEG_INFINITY);
    for (triangle, face) in indices.chunks_exact(3).enumerate() {
        checkpoint(progress, triangle)?;
        if !reliable[triangle] || !keep.contains(&components.root(face[0])) { continue; }
        if face.iter().any(|&index| {
            let point = positions[index as usize];
            point.cmplt(safety_min).any() || point.cmpgt(safety_max).any()
        }) { continue; }
        retained[triangle] = true;
        for &index in face { minimum = minimum.min(positions[index as usize]); maximum = maximum.max(positions[index as usize]); }
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        diagnostics.push("SubjectFraming:EmptySelection:FullMeshFallback".to_owned());
        return Ok(fallback());
    }
    let mut selected = Vec::with_capacity(indices.len());
    let mut selected_ranges = Vec::with_capacity(ranges.len());
    let mut copy_range = |start: usize, count: usize, selected: &mut Vec<u32>| -> CoreResult<()> {
        for triangle in start / 3..(start + count) / 3 {
            checkpoint(progress, triangle)?;
            if retained[triangle] { selected.extend_from_slice(&indices[triangle * 3..triangle * 3 + 3]); }
        }
        Ok(())
    };
    if ranges.is_empty() {
        copy_range(0, indices.len(), &mut selected)?;
    } else {
        for range in ranges {
            let start = selected.len();
            copy_range(range.start, range.count, &mut selected)?;
            selected_ranges.push(MaterialRange { start, count: selected.len() - start, material_index: range.material_index });
        }
    }
    diagnostics.push("SubjectFraming:DominantSurface".to_owned());
    if has_body_support { diagnostics.push("SubjectFraming:BodyBoneWeights".to_owned()); }
    let excluded = indices.len() / 3 - selected.len() / 3;
    if excluded > 0 { diagnostics.push(format!("SubjectFraming:ExcludedTriangles:{excluded}")); }
    if excluded_components > 0 { diagnostics.push(format!("SubjectFraming:ExcludedComponents:{excluded_components}")); }
    if invisible_count > 0 { diagnostics.push(format!("SubjectFraming:InvisibleTriangles:{invisible_count}")); }
    Ok(SubjectFrame { minimum, maximum, indices: selected, ranges: selected_ranges })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(origin: Vec3, size: Vec3, columns: u32, rows: u32) -> (Vec<SkinnedVertex>, Vec<u32>) {
        let mut vertices = Vec::new(); let mut indices = Vec::new();
        for y in 0..=rows { for x in 0..=columns {
            vertices.push(SkinnedVertex { position: origin + Vec3::new(size.x * (x as f32 / columns as f32),
                size.y * (y as f32 / rows as f32), 0.0), normal: Vec3::NEG_Z, color: [1.0; 4] });
            if x < columns && y < rows {
                let a = y * (columns + 1) + x; let b = a + columns + 1;
                indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
        } }
        (vertices, indices)
    }

    fn frame(vertices: &[SkinnedVertex], indices: &[u32], weights: &[f32]) -> SubjectFrame {
        subject_frame(vertices, indices, &[], &[], weights, &mut Vec::new(), &mut |_, _| true).unwrap()
    }

    fn append(vertices: &mut Vec<SkinnedVertex>, indices: &mut Vec<u32>, mesh: (Vec<SkinnedVertex>, Vec<u32>)) {
        let offset = vertices.len() as u32;
        vertices.extend(mesh.0); indices.extend(mesh.1.into_iter().map(|index| index + offset));
    }

    #[test]
    fn unreferenced_extreme_vertices_do_not_change_the_subject() {
        let (mut vertices, indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        let expected = frame(&vertices, &indices, &[]);
        vertices.push(SkinnedVertex { position: Vec3::splat(f32::MAX / 4.0), normal: Vec3::Y, color: [1.0; 4] });
        let actual = frame(&vertices, &indices, &[]);
        assert_eq!(actual.minimum, expected.minimum); assert_eq!(actual.maximum, expected.maximum);
        assert_eq!(actual.indices, indices);
    }

    #[test]
    fn detached_remote_faces_and_connected_flying_vertices_are_omitted() {
        let (mut vertices, mut indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        let original = indices.clone();
        append(&mut vertices, &mut indices, plane(Vec3::new(1000.0, 0.0, 0.0), Vec3::ONE, 1, 1));
        let detached = frame(&vertices, &indices, &[]);
        assert_eq!(detached.indices, original); assert!(detached.maximum.x < 9.0);
        vertices[0].position.x = 100_000.0;
        let attached = frame(&vertices, &indices, &[]);
        assert!(!attached.indices.contains(&0)); assert!(attached.maximum.x < 9.0);
        assert!(attached.indices.len() >= original.len() - 9);
    }

    #[test]
    fn nearby_disconnected_hair_skirts_wings_and_uv_seams_stay_in_the_frame() {
        let (mut vertices, mut indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        append(&mut vertices, &mut indices, plane(Vec3::new(7.5, 4.0, 0.05), Vec3::new(12.0, 14.0, 0.0), 2, 2));
        append(&mut vertices, &mut indices, plane(Vec3::new(-3.0, -3.0, 0.03), Vec3::new(14.0, 8.0, 0.0), 2, 2));
        append(&mut vertices, &mut indices, plane(Vec3::new(0.0, 15.0, 0.02), Vec3::new(8.0, 12.0, 0.0), 2, 2));
        let actual = frame(&vertices, &indices, &[]);
        assert_eq!(actual.indices, indices);
        assert!(actual.minimum.x <= -3.0 && actual.maximum.x >= 19.5 && actual.maximum.y > 26.0);
    }

    #[test]
    fn body_weights_prefer_the_character_over_a_denser_remote_mesh() {
        let (mut vertices, mut indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        let body_count = vertices.len(); let expected = indices.clone();
        append(&mut vertices, &mut indices, plane(Vec3::new(1000.0, 0.0, 0.0), Vec3::new(20.0, 30.0, 0.0), 100, 200));
        let mut weights = vec![0.0; vertices.len()]; weights[..body_count].fill(1.0);
        let actual = frame(&vertices, &indices, &weights);
        assert_eq!(actual.indices, expected); assert!(actual.maximum.x <= 8.0);
    }

    #[test]
    fn invisible_materials_do_not_pull_the_camera_and_material_groups_are_preserved() {
        let (mut vertices, mut indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        let body_indices = indices.len();
        append(&mut vertices, &mut indices, plane(Vec3::new(1000.0, 0.0, 0.0), Vec3::new(80.0, 160.0, 0.0), 8, 16));
        let model = super::super::parse_pmx_model(&super::super::test_fixtures::pmx(false, 1, 0, false)).unwrap();
        let bytes = mmd_anim_format::export_pmx_model(&model);
        let mut materials = super::super::render_input_from_pmx(&bytes, &model).unwrap().materials;
        materials[0].diffuse[3] = 0.0;
        materials.extend(super::super::render_input_from_pmx(&bytes, &model).unwrap().materials);
        let ranges = [MaterialRange { start: 0, count: body_indices, material_index: 1 },
            MaterialRange { start: body_indices, count: indices.len() - body_indices, material_index: 0 }];
        let actual = subject_frame(&vertices, &indices, &ranges, &materials, &[], &mut Vec::new(), &mut |_, _| true).unwrap();
        assert_eq!(actual.indices.len(), body_indices); assert_eq!(actual.ranges[0].material_index, 1);
        assert_eq!(actual.ranges[1].start, body_indices); assert_eq!(actual.ranges[1].count, 0);
    }

    #[test]
    fn selection_is_scale_independent_and_extreme_bounds_stay_finite() {
        let (vertices, indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        for scale in [1.0e-9, 1.0e-3, 1.0, 1.0e6] {
            let scaled = vertices.iter().map(|vertex| SkinnedVertex { position: vertex.position * scale, ..*vertex }).collect::<Vec<_>>();
            let actual = frame(&scaled, &indices, &[]);
            assert_eq!(actual.indices, indices); assert!((actual.maximum.x / f64::from(scale) - 8.0).abs() < 1.0e-5);
        }
        let (extreme, indices) = plane(Vec3::ZERO, Vec3::new(f32::MAX / 4.0, f32::MAX / 4.0, 0.0), 8, 16);
        let actual = frame(&extreme, &indices, &[]);
        assert!(actual.minimum.is_finite() && actual.maximum.is_finite());
        assert!((actual.maximum - actual.minimum).is_finite());
    }

    #[test]
    fn invalid_geometry_fails_and_analysis_can_be_cancelled() {
        let (mut vertices, indices) = plane(Vec3::ZERO, Vec3::new(8.0, 16.0, 0.0), 8, 16);
        assert!(indexed_bounds(&vertices, &[u32::MAX, 0, 1]).is_err());
        assert!(matches!(subject_frame(&vertices, &indices, &[], &[], &[], &mut Vec::new(), &mut |_, _| false),
            Err(CoreError::ThumbnailCancelled)));
        vertices[0].position.x = f32::NAN;
        assert!(indexed_bounds(&vertices, &indices).is_err());
    }

    #[test]
    fn bone_hints_use_names_and_weights_without_skeleton_coordinate_bounds() {
        for name in ["頭", "左足", "上半身2", "Leg", "LowerBody", "RightFoot", "mixamorig:Spine1"] {
            assert!(body_bone(name, ""), "{name}");
        }
        for name in ["センター", "全ての親", "HairRoot", "HeadHair", "Prop", "足ＩＫ"] {
            assert!(!body_bone(name, ""), "{name}");
        }
        let bytes = super::super::test_fixtures::pmx(false, 1, 1, false);
        let mut model = super::super::parse_pmx_model(&bytes).unwrap();
        let expected = pmx_body_weights(&model);
        for bone in &mut model.skeleton.bones { bone.position = [1.0e20; 3]; }
        assert_eq!(pmx_body_weights(&model), expected); assert!(expected.iter().all(|&weight| weight == 0.5));
        let pmd = super::super::parse_pmd_model(&super::super::test_fixtures::pmd(false, "")).unwrap();
        assert!(pmd_body_weights(&pmd).iter().all(|&weight| weight == 0.5));
    }

    #[test]
    fn empty_visibility_and_degenerate_surfaces_fall_back_without_panicking() {
        let vertices = vec![SkinnedVertex { position: Vec3::ZERO, normal: Vec3::Y, color: [1.0; 4] }; 3];
        let actual = frame(&vertices, &[0, 1, 2], &[]);
        assert_eq!(actual.indices, [0, 1, 2]); assert_eq!(actual.minimum, DVec3::ZERO);
        let model = super::super::parse_pmx_model(&super::super::test_fixtures::pmx(false, 1, 0, false)).unwrap();
        let bytes = mmd_anim_format::export_pmx_model(&model);
        let mut input = super::super::render_input_from_pmx(&bytes, &model).unwrap(); input.materials[0].diffuse[3] = 0.0;
        let actual = subject_frame(&input.vertices, &input.indices, &input.material_ranges, &input.materials,
            &input.subject_weights, &mut Vec::new(), &mut |_, _| true).unwrap();
        assert_eq!(actual.indices, input.indices);
    }
}
