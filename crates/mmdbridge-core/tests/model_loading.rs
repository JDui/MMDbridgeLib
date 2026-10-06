mod fixtures;

use std::path::PathBuf;
use mmdbridge_core::{AssetType, Library};

struct FixtureDirectory(PathBuf);
impl FixtureDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mmdbridge-model-雪-テスト-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap(); Self(path)
    }
}
impl Drop for FixtureDirectory {
    fn drop(&mut self) { assert!(self.0.is_absolute() && self.0.starts_with(std::env::temp_dir())); std::fs::remove_dir_all(&self.0).unwrap(); }
}

#[test]
fn scan_tags_are_repeatable_replace_old_facts_and_respect_user_removals() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    let path = directory.0.join("裙子蓝色.pmx"); std::fs::write(&path, fixtures::pmx(false,1,3,false)).unwrap();
    let root = library.add_root(AssetType::Model, directory.0.to_str().unwrap(), None).unwrap();
    library.scan_root(&root.id).unwrap();
    let asset = library.list_assets(None, None, 10).unwrap().remove(0);
    let tags = library.list_asset_tags(&asset.id).unwrap();
    assert!(tags.iter().any(|tag| tag.name == "技术:含SDEF" && tag.source == "parser"));
    assert!(!tags.iter().any(|tag| tag.name.starts_with("裙型:") || tag.name.starts_with("整体色:")));
    assert_eq!(library.scan_root(&root.id).unwrap().assets_unchanged, 1);
    assert_eq!(serde_json::to_value(&tags).unwrap(), serde_json::to_value(library.list_asset_tags(&asset.id).unwrap()).unwrap());
    library.remove_asset_tag(&asset.id, "技术:含SDEF").unwrap();
    library.add_asset_tag(&asset.id, "服装:半身裙", "agent", Some(0.85)).unwrap();
    std::fs::write(&path, fixtures::pmx(false,1,0,true)).unwrap();
    library.scan_root(&root.id).unwrap();
    let tags = library.list_asset_tags(&asset.id).unwrap();
    assert!(!tags.iter().any(|tag| tag.name == "技术:含SDEF"));
    assert!(tags.iter().any(|tag| tag.name == "服装:半身裙" && tag.source == "agent"));
    std::fs::write(&path, fixtures::pmx(false,1,3,true)).unwrap(); library.scan_root(&root.id).unwrap();
    assert!(!library.list_asset_tags(&asset.id).unwrap().iter().any(|tag| tag.name == "技术:含SDEF"));
    std::fs::write(&path, b"corrupt").unwrap(); library.scan_root(&root.id).unwrap();
    assert!(!library.list_asset_tags(&asset.id).unwrap().iter().any(|tag| tag.source == "parser"));
    assert!(library.list_asset_tags(&asset.id).unwrap().iter().any(|tag| tag.source == "agent"));
}

#[test]
fn enabling_scan_tags_backfills_unchanged_assets_without_reparsing() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    library.set_auto_tag_settings(&mmdbridge_core::AutoTagSettings { technical:false, colors:false }).unwrap();
    let path = directory.0.join("旧模型.pmd"); std::fs::write(&path, fixtures::pmd(false, "")).unwrap();
    let root = library.add_root(AssetType::Model, directory.0.to_str().unwrap(), None).unwrap();
    library.scan_root(&root.id).unwrap(); let asset = library.list_assets(None, None, 10).unwrap().remove(0);
    assert!(library.list_asset_tags(&asset.id).unwrap().is_empty());
    library.set_auto_tag_settings(&mmdbridge_core::AutoTagSettings::default()).unwrap();
    assert_eq!(library.scan_root(&root.id).unwrap().assets_unchanged, 1);
    assert!(library.list_asset_tags(&asset.id).unwrap().iter().any(|tag| tag.name == "格式:PMD"));
    assert!(!library.list_asset_tags(&asset.id).unwrap().iter().any(|tag| tag.name == "技术:含Morph"));
}

#[test]
fn pmx_encodings_index_widths_and_weight_modes_load_without_gpu() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    for version in [2.0f32,2.1] { for utf16 in [false,true] { for width in [1,2,4] { for mode in 0..=4 {
        if version == 2.0 && mode == 4 { continue; }
        let path = directory.0.join(format!("モデル-{version}-{utf16}-{width}-{mode}.pmx"));
        let mut bytes = fixtures::pmx(utf16,width,mode,true);
        bytes[4..8].copy_from_slice(&version.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let preview = library.model_preview_file(&path).unwrap();
        assert_eq!(&preview[..4], b"MMDV"); assert_eq!(u32::from_le_bytes(preview[8..12].try_into().unwrap()), 3);
    } } } }
}

#[test]
fn pmd_model_scan_viewer_and_texture_reference_share_the_same_format_support() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    let path = directory.0.join("旧模型.PMD"); std::fs::write(&path, fixtures::pmd(true,"env.sph*tex.png")).unwrap();
    let root = library.add_root(AssetType::Model, directory.0.to_str().unwrap(), None).unwrap();
    let report = library.scan_root(&root.id).unwrap(); assert_eq!(report.parse_failures, 0);
    let assets = library.list_assets(Some(AssetType::Model), None, 20).unwrap(); assert_eq!(assets.len(), 1);
    let asset = library.inspect_asset(&assets[0].id).unwrap();
    assert_eq!(asset.metadata["file_type"], "pmd"); assert_eq!(asset.metadata["vertex_count"], 3);
    assert!(AssetType::Model.supports_thumbnail_extension("PMD"));
    let preview = library.model_preview(&asset.id).unwrap();
    assert_eq!(u32::from_le_bytes(preview[20..24].try_into().unwrap()), 2);
    assert!(preview.windows(7).any(|value| value == b"tex.png"));
    assert!(library.model_preview_texture_file(&path, "tex.png").unwrap().is_none());
    assert!(library.model_preview_texture_file(&path, "unreferenced.png").is_err());
}

#[test]
fn truncated_legacy_models_and_oversized_counts_fail_without_allocating_declared_arrays() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    let path = directory.0.join("損坏.pmd"); let valid = fixtures::pmd(false, "");
    for length in 0..valid.len() {
        std::fs::write(&path, &valid[..length]).unwrap();
        assert!(library.model_preview_file(&path).is_err(), "truncation at {length}");
    }
    for offset in [283, 283 + 4 + 3 * 38, 283 + 4 + 3 * 38 + 4 + 3 * 2] {
        let mut corrupt = valid.clone(); corrupt[offset..offset+4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, corrupt).unwrap(); assert!(library.model_preview_file(&path).is_err());
    }
    let path = directory.0.join("損坏.pmx"); let mut pmx = fixtures::pmx(false,1,0,false); pmx[10] = 255;
    std::fs::write(&path, pmx).unwrap(); assert!(library.model_preview_file(&path).is_err());
}

#[test]
fn legacy_pmd_can_be_selected_for_motion_but_an_unusable_runtime_cannot() {
    let directory = FixtureDirectory::new(); let library = Library::in_memory().unwrap();
    let path = directory.0.join("旧模型.pmd"); std::fs::write(&path, fixtures::pmd(false, "")).unwrap();
    assert!(library.set_motion_preview_model(Some(path.to_str().unwrap())).unwrap().is_some());
    let saved = library.motion_preview_model().unwrap();
    let path = directory.0.join("循环骨架.pmx");
    let mut parsed = mmd_anim_format::parse_pmx_model(&fixtures::pmx(false,1,0,false)).unwrap();
    parsed.skeleton.bones[0].parent_index = 1;
    std::fs::write(&path, mmd_anim_format::export_pmx_model(&parsed)).unwrap();
    assert!(library.set_motion_preview_model(Some(path.to_str().unwrap())).is_err());
    assert_eq!(library.motion_preview_model().unwrap(), saved);
}
