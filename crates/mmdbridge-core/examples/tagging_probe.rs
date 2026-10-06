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
    image::RgbaImage::from_pixel(64, 64, image::Rgba([28, 82, 235, 255])).save(models.join("blue.png"))?;
    let mut vertices = Vec::new(); let mut indices = Vec::new();
    let (rings, sectors) = (32u32, 64u32);
    for ring in 0..=rings { for sector in 0..=sectors {
        let v = ring as f32 / rings as f32; let u = sector as f32 / sectors as f32;
        let theta = v * std::f32::consts::PI; let phi = u * std::f32::consts::TAU;
        let normal = [theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()];
        vertices.push(([normal[0] * 3.0, normal[1] * 5.0 + 5.0, normal[2] * 3.0], normal, [u, v]));
        if ring < rings && sector < sectors {
            let a = ring * (sectors + 1) + sector; let b = a + sectors + 1;
            indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    } }
    for (name, texture) in [("蓝色材质测试", "blue.png"), ("缺失贴图测试", "missing.png")] {
        let mut parsed = mmd_anim_format::parse_pmx_model(&fixtures::pmx_mesh(false, 2, 3, false, &vertices, &indices))?;
        parsed.metadata.name = name.to_owned(); parsed.materials[0].texture_path = texture.to_owned();
        parsed.materials[0].diffuse = [1.0, 1.0, 1.0, 1.0];
        fs::write(models.join(format!("{name}.pmx")), mmd_anim_format::export_pmx_model(&parsed))?;
    }
    let library = Library::in_memory()?;
    let root = library.add_root(AssetType::Model, models.to_str().ok_or("invalid path")?, None)?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures, 0);
    let mut entries = Vec::new();
    for asset in library.list_assets(Some(AssetType::Model), None, 10)? {
        let before = library.list_asset_tags(&asset.id)?;
        assert!(before.iter().any(|tag| tag.name == "技术:含SDEF"));
        assert!(!before.iter().any(|tag| tag.name.starts_with("整体色:")));
        library.create_card_with_thumbnail(&asset.id)?;
        let after = library.list_asset_tags(&asset.id)?;
        let missing = asset.name.starts_with("缺失");
        assert_eq!(after.iter().any(|tag| tag.name == "整体色:蓝色"), !missing);
        if missing { assert!(!after.iter().any(|tag| tag.name.starts_with("整体色:"))); }
        let preview = library.card_thumbnail(&asset.id)?.ok_or("thumbnail absent")?;
        fs::write(output.join(format!("{}.webp", asset.id)), &preview)?;
        // A tag-only stale card must reuse the exact preview, not trigger a GPU render.
        library.add_asset_tag(&asset.id, "测试:已检查", "user", None)?;
        library.create_card_with_thumbnail(&asset.id)?;
        assert_eq!(library.card_thumbnail(&asset.id)?.unwrap(), preview);
        assert_eq!(library.verify_card(&asset.id)?.status, "CardValid");
        let mut display = serde_json::to_value(library.inspect_asset(&asset.id)?)?;
        display["primarySource"] = json!(format!("E:\\MMD\\Tests\\{}.pmx", asset.name));
        display["assetDirectory"] = json!("E:\\MMD\\Tests");
        entries.push(json!({"asset":display,"beforeTags":before,"afterTags":after}));
        if !missing {
            library.remove_asset_tag(&asset.id, "整体色:蓝色")?;
            library.create_card_with_thumbnail(&asset.id)?;
            assert!(!library.list_asset_tags(&asset.id)?.iter().any(|tag| tag.name == "整体色:蓝色"));
            library.add_asset_tag(&asset.id, "整体色:红色", "user", None)?;
            library.create_card_with_thumbnail(&asset.id)?;
            assert_eq!(library.card_thumbnail(&asset.id)?.unwrap(), preview);
            assert!(library.list_asset_tags(&asset.id)?.iter().any(|tag| tag.name == "整体色:红色" && tag.source == "user"));
        }
    }
    assert_eq!(library.scan_root(&root.id)?.assets_unchanged, 2);
    fs::write(output.join("manifest.json"), serde_json::to_vec_pretty(&json!({
        "synthetic":true,"entries":entries,"checks":{"technicalAtScan":true,"coloursAfterThumbnail":true,"missingTextureSkipped":true,"userRemovalRespected":true,"userPalettePreserved":true,"previewReusedForTagChanges":true,"cardValid":true,"unchangedRescan":true}
    }))?)?;
    Ok(())
}
