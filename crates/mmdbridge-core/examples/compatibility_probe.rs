use std::{error::Error, fs, path::PathBuf};
use mmdbridge_core::{AssetType, Library};
use serde_json::json;

#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(std::env::args_os().nth(1).ok_or("missing new output directory")?);
    if output.exists() { return Err("probe output must be a new directory".into()); }
    fs::create_dir(&output)?;
    let models = output.join("models"); fs::create_dir(&models)?;
    let textures = models.join("貼図"); fs::create_dir(&textures)?;
    let mut diffuse = image::RgbaImage::new(256,256);
    let mut subtexture = image::RgbaImage::new(256,256);
    let mut cutout = image::RgbaImage::new(256,256);
    for y in 0..256 { for x in 0..256 {
        let stripe = (x/32 + y/32) % 2 == 0;
        let color = if stripe { [206,157,84,255] } else { [51,115,128,255] };
        diffuse.put_pixel(x,y,image::Rgba(color));
        subtexture.put_pixel(x,y,image::Rgba([x as u8, y as u8, 170,255]));
        cutout.put_pixel(x,y,image::Rgba([88,176,182, if stripe {255} else {0}]));
    } }
    diffuse.save(textures.join("tex.png"))?; subtexture.save(models.join("uv.png"))?;
    diffuse.save(models.join("tex.png"))?; cutout.save(models.join("cutout.png"))?;
    image::RgbaImage::from_pixel(32,32,image::Rgba([180,195,210,255]))
        .save_with_format(models.join("env.sph"), image::ImageFormat::Bmp)?;
    let mut vertices = Vec::new(); let mut indices = Vec::new();
    let (rings, sectors) = (32u32,64u32);
    for ring in 0..=rings { for sector in 0..=sectors {
        let v = ring as f32 / rings as f32; let u = sector as f32 / sectors as f32;
        let theta = v * std::f32::consts::PI; let phi = u * std::f32::consts::TAU;
        let normal = [theta.sin()*phi.cos(),theta.cos(),theta.sin()*phi.sin()];
        vertices.push(([normal[0]*4.0,normal[1]*4.0+4.0,normal[2]*4.0],normal,[u,v]));
        if ring < rings && sector < sectors {
            let a = ring*(sectors+1)+sector; let b = a+sectors+1;
            indices.extend([a,b,a+1,a+1,b,b+1]);
        }
    } }
    for (name, mode, material) in [("PMX-Diffuse",0,0),("PMX-Subtexture-UV1",0,1),("PMX-SDEF",3,0),("PMX-Cutout",0,2)] {
        let bytes = fixtures::pmx_mesh(false,2,mode,true,&vertices,&indices);
        let mut model = mmd_anim_format::parse_pmx_model(&bytes)?; model.metadata.name = name.to_owned();
        if material == 1 { model.materials[0].sphere_mode = "subTexture".to_owned(); model.materials[0].sphere_texture_path = "uv.png".to_owned(); }
        if material == 2 { model.materials[0].texture_path = "cutout.png".to_owned(); }
        fs::write(models.join(format!("{name}.pmx")),mmd_anim_format::export_pmx_model(&model))?;
    }
    let mut pmd = mmd_anim_format::parse_pmd_model(&fixtures::pmd(false,"env.sph*tex.png"))?;
    let template = pmd.geometry.vertices[0].clone();
    pmd.geometry.vertices = vertices.iter().map(|(position,normal,uv)| {
        let mut vertex = template.clone(); vertex.position = *position; vertex.normal = *normal; vertex.uv = *uv;
        vertex.bone_weight = ((1.0-(position[1]/8.0))*100.0).round() as u8; vertex
    }).collect();
    pmd.geometry.indices = indices.iter().map(|value| *value as u16).collect();
    pmd.materials[0].face_count = (indices.len()/3) as u32;
    pmd.metadata.name = "PMD-Sphere".to_owned(); pmd.metadata.name_bytes.clear();
    let pmd_path = models.join("PMD-Sphere.pmd"); fs::write(&pmd_path,mmd_anim_format::export_pmd_model(&pmd))?;
    let library = Library::in_memory()?;
    let root = library.add_root(AssetType::Model,models.to_str().ok_or("invalid model path")?,None)?;
    let scan = library.scan_root(&root.id)?; assert_eq!(scan.parse_failures,0);
    let mut manifest = Vec::new();
    for asset in library.list_assets(Some(AssetType::Model),None,20)? {
        let thumbnail = library.render_thumbnail(&asset.id)?;
        assert_eq!(thumbnail.report.triangle_count,indices.len()/3);
        assert!(thumbnail.report.texture_count > 0, "textures must load on Linux using Windows separators");
        assert!(!thumbnail.report.diagnostics.iter().any(|value| value.starts_with("MissingTexture:") || value.starts_with("TextureLoadFailed:")));
        fs::write(output.join(format!("{}.webp",asset.id)),&thumbnail.preview_webp)?;
        fs::write(output.join(format!("{}.bin",asset.id)),library.model_preview(&asset.id)?)?;
        library.create_card_with_thumbnail(&asset.id)?;
        assert_eq!(library.verify_card(&asset.id)?.status,"CardValid");
        let preserved = library.card_thumbnail(&asset.id)?.ok_or("generated card preview absent")?;
        library.create_card_with_thumbnail(&asset.id)?;
        assert_eq!(library.card_thumbnail(&asset.id)?.ok_or("refreshed card preview absent")?, preserved);
        let mut display = serde_json::to_value(library.inspect_asset(&asset.id)?)?;
        display["primarySource"] = json!(format!("E:\\MMD\\Tests\\{}", PathBuf::from(&asset.primary_source).file_name().unwrap().to_string_lossy()));
        display["assetDirectory"] = json!("E:\\MMD\\Tests");
        display["hasThumbnail"] = json!(true);
        manifest.push(json!({"asset":display,"report":thumbnail.report}));
    }
    let motions = output.join("motions"); fs::create_dir(&motions)?;
    let mut vmd = b"Vocaloid Motion Data 0002".to_vec(); vmd.resize(30,0); vmd.extend([0;20]);
    vmd.extend(1u32.to_le_bytes()); let mut name = [0u8;15]; name[..4].copy_from_slice(b"head"); vmd.extend(name);
    vmd.extend(10u32.to_le_bytes()); fixtures::floats(&mut vmd,&[2.0,0.0,0.0,0.0,0.0,0.0,1.0]); vmd.extend([20;64]);
    for _ in 0..5 { vmd.extend(0u32.to_le_bytes()); }
    fs::write(motions.join("PMD-Motion.vmd"),vmd)?;
    fs::write(motions.join("PMD-Pose.vpd"),b"Vocaloid Pose Data file\n\nFixture.osm;\n1;\n\nBone0{head\n 2,0,0;\n 0,0,0,1;\n}\n")?;
    library.set_motion_preview_model(Some(pmd_path.to_str().ok_or("invalid PMD path")?))?;
    let root = library.add_root(AssetType::Motion,motions.to_str().ok_or("invalid motion path")?,None)?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures,0);
    for asset in library.list_assets(Some(AssetType::Motion),None,20)? {
        if asset.primary_source.to_ascii_lowercase().ends_with(".vmd") {
            let frame = library.motion_preview_frame(&asset.id,10)?;
            assert_eq!(frame["frame"],10); assert_eq!(frame["maxFrame"],10);
            assert_eq!(frame["boneMatrices"].as_array().ok_or("missing bone matrices")?.len(),32);
        }
        let thumbnail = library.render_thumbnail(&asset.id)?;
        assert!(thumbnail.report.diagnostics.iter().any(|value| value.starts_with("PmdMotionPreview:")));
        fs::write(output.join(format!("{}.webp",asset.id)),&thumbnail.preview_webp)?;
        let mut display = serde_json::to_value(&asset)?; display["hasThumbnail"] = json!(true);
        manifest.push(json!({"asset":display,"report":thumbnail.report}));
    }
    let png = library.model_preview_texture_file(&pmd_path,"tex.png")?.ok_or("PMD texture absent")?;
    fs::write(output.join("preview-texture.png"),png.0)?;
    let (cutout_png,alpha) = library.model_preview_texture_file(&models.join("PMX-Cutout.pmx"),"cutout.png")?.ok_or("cutout texture absent")?;
    let mut texture_bytes = vec![alpha]; texture_bytes.extend(cutout_png);
    fs::write(output.join("viewer-cutout-texture.bin"),texture_bytes)?;
    fs::write(output.join("manifest.json"),serde_json::to_vec_pretty(&json!({"synthetic":true,"entries":manifest}))?)?;
    write_viewer_fixtures(&output, &vertices, &indices)?;
    println!("{}",serde_json::to_string(&json!({"synthetic":true,"rendered":manifest.len(),"models":5,"pmdMotionPreviews":2}))?);
    Ok(())
}

fn write_viewer_fixtures(output: &std::path::Path,
    sphere: &[([f32;3],[f32;3],[f32;2])], sphere_indices: &[u32]) -> Result<(), Box<dyn Error>> {
    let folder = output.join("viewer-fixtures"); fs::create_dir(&folder)?;
    let mut vertices = Vec::new(); let mut indices = Vec::new();
    for (centre, scale) in [
        ([0.0,18.5,0.0],[2.0,2.1,1.65]), ([0.0,12.5,0.0],[2.2,3.9,1.15]),
        ([0.0,8.9,0.0],[2.5,1.5,1.25]), ([-3.0,12.0,0.0],[0.7,3.3,0.7]),
        ([3.0,12.0,0.0],[0.7,3.3,0.7]), ([-1.15,4.6,0.0],[0.7,4.25,0.75]),
        ([1.15,4.6,0.0],[0.7,4.25,0.75]), ([-1.15,0.5,-0.45],[0.8,0.5,1.3]),
        ([1.15,0.5,-0.45],[0.8,0.5,1.3]),
    ] {
        let offset = vertices.len() as u32;
        for (_, normal, uv) in sphere {
            let position = std::array::from_fn(|axis| centre[axis] + normal[axis] * scale[axis]);
            let mut transformed: [f32;3] = std::array::from_fn(|axis| normal[axis] / scale[axis]);
            let length = transformed.iter().map(|value| value * value).sum::<f32>().sqrt();
            transformed.iter_mut().for_each(|value| *value /= length);
            vertices.push((position, transformed, *uv));
        }
        for face in sphere_indices.chunks_exact(3) { indices.extend([face[0]+offset,face[2]+offset,face[1]+offset]); }
    }
    let pmx_path = folder.join("参照角色.pmx");
    let mut pmx = mmd_anim_format::parse_pmx_model(&fixtures::pmx_mesh(false,2,0,false,&vertices,&indices))?;
    pmx.materials[0].texture_path.clear(); pmx.materials[0].sphere_texture_path.clear();
    fs::write(&pmx_path,mmd_anim_format::export_pmx_model(&pmx))?;
    let pmd_path = folder.join("参照角色.pmd");
    let mut pmd = mmd_anim_format::parse_pmd_model(&fixtures::pmd(false,""))?;
    let template = pmd.geometry.vertices[0].clone();
    pmd.geometry.vertices = vertices.iter().map(|(position,normal,uv)| {
        let mut vertex = template.clone(); vertex.position = *position; vertex.normal = *normal;
        vertex.uv = *uv; vertex.bone_weight = 100; vertex
    }).collect();
    pmd.geometry.indices = indices.iter().map(|index| *index as u16).collect();
    pmd.materials[0].face_count = (indices.len()/3) as u32;
    fs::write(&pmd_path,mmd_anim_format::export_pmd_model(&pmd))?;
    let library = Library::in_memory()?;
    fs::write(output.join("viewer-character-pmx.bin"),library.model_preview_file(&pmx_path)?)?;
    fs::write(output.join("viewer-character-pmd.bin"),library.model_preview_file(&pmd_path)?)?;

    let scene_folder = folder.join("scenes"); fs::create_dir(&scene_folder)?;
    let mut room_vertices = Vec::new(); let mut room_indices = Vec::new();
    let mut quad = |points: [[f32;3];4], normal: [f32;3]| {
        let offset = room_vertices.len() as u32;
        for (position,uv) in points.into_iter().zip([[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0]]) {
            room_vertices.push((position,normal,uv));
        }
        room_indices.extend([offset,offset+2,offset+1,offset,offset+3,offset+2]);
    };
    quad([[-32.0,-0.1,-60.0],[32.0,-0.1,-60.0],[32.0,-0.1,32.0],[-32.0,-0.1,32.0]],[0.0,1.0,0.0]);
    quad([[-32.0,0.0,32.0],[32.0,0.0,32.0],[32.0,36.0,32.0],[-32.0,36.0,32.0]],[0.0,0.0,-1.0]);
    quad([[-32.0,0.0,-60.0],[-32.0,0.0,32.0],[-32.0,36.0,32.0],[-32.0,36.0,-60.0]],[1.0,0.0,0.0]);
    quad([[32.0,0.0,32.0],[32.0,0.0,-60.0],[32.0,36.0,-60.0],[32.0,36.0,32.0]],[-1.0,0.0,0.0]);
    let mut room = mmd_anim_format::parse_pmx_model(&fixtures::pmx_mesh(false,2,0,false,&room_vertices,&room_indices))?;
    room.metadata.name = "场景与尺寸参照".to_owned(); room.materials[0].texture_path.clear();
    fs::write(scene_folder.join("场景.pmx"),mmd_anim_format::export_pmx_model(&room))?;
    let root = library.add_root(AssetType::Scene,scene_folder.to_str().ok_or("invalid scene path")?,None)?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures,0);
    let scene = library.list_assets(Some(AssetType::Scene),None,1)?.remove(0);
    fs::write(output.join("viewer-scene.bin"),library.scene_preview(&scene.id)?)?;

    let motion_folder = folder.join("motions"); fs::create_dir(&motion_folder)?;
    let mut vmd = b"Vocaloid Motion Data 0002".to_vec(); vmd.resize(30,0); vmd.extend([0;20]);
    vmd.extend(2u32.to_le_bytes());
    for (frame,x) in [(0u32,0.0),(60u32,3.0)] {
        let mut name = [0u8;15]; name[..4].copy_from_slice(b"root"); vmd.extend(name);
        vmd.extend(frame.to_le_bytes()); fixtures::floats(&mut vmd,&[x,0.0,0.0,0.0,0.0,0.0,1.0]); vmd.extend([20;64]);
    }
    for _ in 0..5 { vmd.extend(0u32.to_le_bytes()); }
    fs::write(motion_folder.join("参照角色动作.vmd"),vmd)?;
    library.set_motion_preview_model(Some(pmd_path.to_str().ok_or("invalid reference path")?))?;
    let root = library.add_root(AssetType::Motion,motion_folder.to_str().ok_or("invalid motion path")?,None)?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures,0);
    let motion = library.list_assets(Some(AssetType::Motion),None,1)?.remove(0);
    let mut frames = Vec::new();
    for frame in 0..=60 {
        let mut value = library.motion_preview_frame(&motion.id,frame)?;
        value["modelPath"] = json!("E:\\MMD\\Tests\\参照角色.pmd"); frames.push(value);
    }
    let mut scene_display = serde_json::to_value(scene)?;
    scene_display["primarySource"] = json!("E:\\MMD\\Tests\\场景.pmx");
    let mut motion_display = serde_json::to_value(motion)?;
    motion_display["primarySource"] = json!("E:\\MMD\\Tests\\参照角色动作.vmd");
    fs::write(output.join("viewer-fixtures.json"),serde_json::to_vec_pretty(&json!({
        "synthetic":true,"scene":scene_display,"motion":motion_display,"frames":frames,
        "referencePmx":"E:\\MMD\\Tests\\参照角色.pmx","referencePmd":"E:\\MMD\\Tests\\参照角色.pmd"
    }))?)?;
    Ok(())
}
