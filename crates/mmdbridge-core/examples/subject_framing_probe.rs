use std::{error::Error, fs, path::PathBuf};

use mmdbridge_core::{AssetType, Library};
use serde_json::json;

#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

type Vertex = ([f32; 3], [f32; 3], [f32; 2]);

fn ellipsoid(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, center: [f32; 3], radius: [f32; 3],
    color: usize, rings: u32, sectors: u32) {
    let start = vertices.len() as u32;
    for ring in 0..=rings { for sector in 0..=sectors {
        let theta = ring as f32 / rings as f32 * std::f32::consts::PI;
        let phi = sector as f32 / sectors as f32 * std::f32::consts::TAU;
        let p = [theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()];
        let normal = glam::Vec3::new(p[0] / radius[0], p[1] / radius[1], p[2] / radius[2]).normalize();
        vertices.push(([center[0] + radius[0] * p[0], center[1] + radius[1] * p[1], center[2] + radius[2] * p[2]],
            normal.to_array(), [(color as f32 + 0.5) / 5.0, 0.5]));
        if ring < rings && sector < sectors {
            let a = start + ring * (sectors + 1) + sector; let b = a + sectors + 1;
            indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    } }
}

fn character() -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new(); let mut indices = Vec::new();
    for (center, radius, color) in [
        ([0.0, 10.5, 0.0], [1.7, 3.0, 0.85], 1),
        ([0.0, 13.5, 0.0], [0.55, 0.9, 0.55], 0),
        ([0.0, 15.0, 0.0], [1.4, 1.65, 1.15], 0),
        ([0.0, 16.4, 0.15], [1.5, 0.5, 1.2], 2),
        ([-1.35, 14.1, 0.55], [0.6, 2.3, 0.75], 2),
        ([1.35, 14.1, 0.55], [0.6, 2.3, 0.75], 2),
        ([-2.15, 9.8, 0.0], [0.5, 2.75, 0.5], 0),
        ([2.15, 9.8, 0.0], [0.5, 2.75, 0.5], 0),
        ([0.0, 7.1, 0.0], [2.1, 1.8, 1.05], 1),
        ([-0.7, 3.5, 0.0], [0.55, 3.45, 0.55], 3),
        ([0.7, 3.5, 0.0], [0.55, 3.45, 0.55], 3),
        ([-0.7, 0.4, -0.25], [0.62, 0.4, 0.9], 4),
        ([0.7, 0.4, -0.25], [0.62, 0.4, 0.9], 4),
        ([-0.45, 15.2, -1.1], [0.16, 0.22, 0.12], 4),
        ([0.45, 15.2, -1.1], [0.16, 0.22, 0.12], 4),
    ] { ellipsoid(&mut vertices, &mut indices, center, radius, color, 12, 24); }
    (vertices, indices)
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(std::env::args_os().nth(1).ok_or("missing new output directory")?);
    if output.exists() { return Err("probe output must be a new directory".into()); }
    fs::create_dir(&output)?; let models = output.join("models"); fs::create_dir(&models)?;
    fs::create_dir(output.join("after"))?; fs::create_dir(output.join("before"))?;
    let textures = models.join("貼図"); fs::create_dir(&textures)?;
    let palette = [[240, 202, 182, 255], [65, 148, 164, 255], [221, 183, 99, 255], [217, 225, 227, 255], [36, 53, 66, 255]];
    let mut texture = image::RgbaImage::new(100, 20);
    for y in 0..20 { for x in 0..100 { texture.put_pixel(x, y, image::Rgba(palette[x as usize / 20])); } }
    texture.save(textures.join("tex.png"))?;
    let (base_vertices, base_indices) = character();
    let cases = [("normal", "标准角色"), ("huge-rig", "巨大控制骨架"), ("unreferenced", "未使用远处顶点"),
        ("detached", "远处碎片"), ("flying", "面片顶点飞远"), ("transparent", "透明远处几何"),
        ("hair-wings", "长发裙摆与翅膀"), ("pmd-flying", "PMD 飛点"), ("dense-prop", "高密度远处配件"),
        ("tiny", "极小比例角色"), ("large", "极大比例角色")];
    for (id, name) in cases {
        let mut vertices = base_vertices.clone(); let mut indices = base_indices.clone();
        match id {
            "unreferenced" => { vertices.push(([100_000.0, 100_000.0, 0.0], [0.0, 0.0, -1.0], [0.1, 0.5])); }
            "detached" => { ellipsoid(&mut vertices, &mut indices, [1000.0, 0.0, 0.0], [0.3; 3], 2, 8, 12); }
            "flying" | "pmd-flying" => { vertices[100].0[0] = 100_000.0; }
            "transparent" => { ellipsoid(&mut vertices, &mut indices, [1000.0, 0.0, 0.0], [40.0; 3], 2, 24, 48); }
            "dense-prop" => { ellipsoid(&mut vertices, &mut indices, [1000.0, 0.0, 0.0], [4.0, 5.0, 4.0], 2, 64, 128); }
            "hair-wings" => {
                for x in [-2.2, 2.2] { ellipsoid(&mut vertices, &mut indices, [x, 9.8, 0.8], [0.85, 7.0, 0.65], 2, 12, 24); }
                ellipsoid(&mut vertices, &mut indices, [0.0, 6.8, 0.0], [4.3, 2.8, 1.4], 1, 12, 24);
                for x in [-5.0, 5.0] { ellipsoid(&mut vertices, &mut indices, [x, 12.0, 1.3], [5.0, 3.0, 0.3], 3, 8, 16); }
            }
            "tiny" | "large" => {
                let scale = if id == "tiny" { 1.0e-7 } else { 1.0e6 };
                for vertex in &mut vertices { for value in &mut vertex.0 { *value *= scale; } }
            }
            _ => {}
        }
        let bytes = fixtures::pmx_mesh(false, 2, 0, true, &vertices, &indices);
        let mut pmx = mmd_anim_format::parse_pmx_model(&bytes)?;
        pmx.metadata.name = name.to_owned(); pmx.skeleton.bones[0].name = "下半身".to_owned();
        pmx.skeleton.bones[0].english_name = "lowerbody".to_owned();
        pmx.skeleton.bones[1].name = "配件".to_owned(); pmx.skeleton.bones[1].english_name = "prop".to_owned();
        pmx.materials[0].diffuse = [1.0; 4]; pmx.materials[0].shared_toon_index = None;
        pmx.materials[0].toon_texture_path.clear(); pmx.materials[0].flags.vertex_color = id == "transparent";
        pmx.geometry.additional_uvs[0] = vertices.iter().enumerate().flat_map(|(index, _)| {
            [1.0, 1.0, 1.0, if id == "transparent" && index >= base_vertices.len() { 0.0 } else { 1.0 }]
        }).collect();
        if id == "huge-rig" { pmx.skeleton.bones[1].position = [0.0, 1.0e6, 0.0]; }
        if id == "dense-prop" { for index in base_vertices.len()..vertices.len() { pmx.geometry.skin_indices[index * 4] = 1; } }
        if id == "pmd-flying" {
            let mut pmd = mmd_anim_format::parse_pmd_model(&fixtures::pmd(false, "tex.png"))?;
            let template = pmd.geometry.vertices[0].clone();
            pmd.geometry.vertices = vertices.iter().map(|&(position, normal, uv)| {
                let mut vertex = template.clone(); vertex.position = position; vertex.normal = normal; vertex.uv = uv;
                vertex.bone_weight = 100; vertex
            }).collect();
            pmd.geometry.indices = indices.iter().map(|&index| index as u16).collect();
            pmd.materials[0].face_count = (indices.len() / 3) as u32; pmd.materials[0].diffuse = [1.0; 4];
            pmd.skeleton.bones[0].name = "lowerbody".to_owned(); pmd.skeleton.bones[0].name_bytes.clear();
            pmd.metadata.name = name.to_owned(); pmd.metadata.name_bytes.clear();
            fs::copy(textures.join("tex.png"), models.join("tex.png"))?;
            fs::write(models.join(format!("{id}.pmd")), mmd_anim_format::export_pmd_model(&pmd))?;
        } else {
            fs::write(models.join(format!("{id}.pmx")), mmd_anim_format::export_pmx_model(&pmx))?;
        }
    }
    let library = Library::in_memory()?;
    let root = library.add_root(AssetType::Model, models.to_str().ok_or("invalid path")?, None)?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures, 0);
    let mut entries = Vec::new();
    for asset in library.list_assets(Some(AssetType::Model), None, 20)? {
        let path = PathBuf::from(&asset.primary_source); let id = path.file_stem().unwrap().to_str().unwrap();
        let start = std::time::Instant::now(); let thumbnail = library.render_thumbnail(&asset.id)?;
        let render_milliseconds = start.elapsed().as_millis();
        assert_eq!(thumbnail.report.renderer_version, "0.6.1");
        assert!(thumbnail.report.diagnostics.iter().any(|value| value == "SubjectFraming:DominantSurface"));
        if ["detached", "flying", "transparent", "pmd-flying", "dense-prop"].contains(&id) {
            assert!(thumbnail.report.diagnostics.iter().any(|value| value.starts_with("SubjectFraming:ExcludedTriangles:")), "{id}");
        } else {
            assert!(!thumbnail.report.diagnostics.iter().any(|value| value.starts_with("SubjectFraming:ExcludedTriangles:")), "{id}");
        }
        fs::write(output.join("after").join(format!("{id}.webp")), &thumbnail.preview_webp)?;
        library.create_card_with_thumbnail(&asset.id)?; assert_eq!(library.verify_card(&asset.id)?.status, "CardValid");
        let mut display = serde_json::to_value(&asset)?; display["hasThumbnail"] = json!(true);
        assert!(!asset.name.contains("&#"), "fixture title must be representable in its source encoding");
        display["primarySource"] = json!(format!("E:\\MMD\\FramingTests\\{}", path.file_name().unwrap().to_string_lossy()));
        display["assetDirectory"] = json!("E:\\MMD\\FramingTests");
        entries.push(json!({"case":id, "modelFile":path.file_name().unwrap().to_string_lossy(), "asset":display,
            "report":thumbnail.report, "renderMilliseconds":render_milliseconds}));
    }
    entries.sort_by_key(|entry| cases.iter().position(|(id, _)| *id == entry["case"].as_str().unwrap()).unwrap());
    fs::write(output.join("manifest.json"), serde_json::to_vec_pretty(&json!({"synthetic":true,
        "baselineCommit":"03ae41203c436032180881c4e8b34b67622a80e8", "entries":entries}))?)?;
    println!("{}", json!({"synthetic":true,"rendered":entries.len(),"renderer":"0.6.1"}));
    Ok(())
}
