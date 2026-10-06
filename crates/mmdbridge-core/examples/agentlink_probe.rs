use std::{error::Error, fs, path::{Path, PathBuf}, process::Command};
use mmdbridge_core::{AgentLinkScope, AssetType, Library};
use serde_json::{Value, json};

#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

fn cli(binary: &Path, arguments: &[&str], success: bool) -> Result<Value, Box<dyn Error>> {
    let output = Command::new(binary).args(arguments).output()?;
    assert_eq!(output.status.success(), success, "{}", String::from_utf8_lossy(&output.stderr));
    let bytes = if output.stdout.is_empty() { &output.stderr } else { &output.stdout };
    Ok(serde_json::from_slice(bytes)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() != 3 { return Err("expected new fixture directory, report directory and CLI binary".into()); }
    let fixture = PathBuf::from(&arguments[0]);
    let output = PathBuf::from(&arguments[1]);
    if fixture.exists() || output.exists() { return Err("probe directories must be new".into()); }
    fs::create_dir_all(fixture.join("data"))?; fs::create_dir_all(&output)?;
    let binary = fixture.join(if cfg!(windows) { "mmdbridge.exe" } else { "mmdbridge" });
    fs::copy(PathBuf::from(&arguments[2]), &binary)?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).ok_or("repository path absent")?;
    let skill = fixture.join("skills/mmdbridge-card-manager");
    for relative in ["SKILL.md", "agents/openai.yaml", "references/tag-taxonomy.md"] {
        let target = skill.join(relative); fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(repository.join("skills/mmdbridge-card-manager").join(relative), target)?;
    }
    let models = fixture.join("模型目录"); fs::create_dir_all(models.join("貼図"))?;
    image::RgbaImage::from_pixel(16, 16, image::Rgba([42, 98, 210, 255])).save(models.join("貼図/tex.png"))?;
    let mut model = mmd_anim_format::parse_pmx_model(&fixtures::pmx(false, 2, 0, false))?;
    model.metadata.name = "几何预览模型".to_owned();
    fs::write(models.join("几何预览模型.pmx"), mmd_anim_format::export_pmx_model(&model))?;
    let library = Library::open(fixture.join("data/library.sqlite3"))?;
    let root = library.add_root(AssetType::Model, models.to_str().ok_or("invalid fixture path")?, Some("模型测试目录".to_owned()))?;
    assert_eq!(library.scan_root(&root.id)?.parse_failures, 0);
    let asset = library.list_assets(Some(AssetType::Model), None, 10)?.remove(0);
    library.create_card_with_thumbnail(&asset.id)?;
    library.add_asset_tag(&asset.id, "用途:参考模型", "user", None)?;
    library.remove_asset_tag(&asset.id, "造型:像素风")?;
    library.create_card(&asset.id, None)?;
    let preview = library.card_thumbnail(&asset.id)?.ok_or("preview absent")?;
    fs::write(output.join("preview.webp"), &preview)?;
    let server = library.start_agent_link()?;
    let waiting = server.snapshot()?;
    assert!(waiting.cli_available && waiting.skill_available);
    let model_waiting = server.set_scope(AgentLinkScope {root_id:None,asset_type:Some(AssetType::Model)})?;
    let scoped_waiting = server.set_scope(AgentLinkScope {root_id:Some(root.id.clone()),asset_type:Some(AssetType::Model)})?;
    let identity = cli(&binary, &["agent-link", "identify", "--live", "--name", "Codex", "--json"], true)?;
    assert_eq!(identity["scope"]["rootId"], root.id);
    let inventory = cli(&binary, &["agent-link", "inspect", "--live", "--limit", "1", "--json"], true)?;
    assert_eq!(inventory["items"].as_array().ok_or("inventory absent")?.len(), 1);
    assert_eq!(inventory["items"][0]["id"], asset.id);
    assert!(inventory["nextCursor"].is_null());
    cli(&binary, &["agent-link", "log", "--live", "--message", "已检查 1 项模型，正在保留现有标签", "--percent", "30", "--json"], true)?;
    for name in ["用途:参考模型", "格式:PMX"] {
        assert_eq!(cli(&binary, &["agent-link", "tags", "--live", "--name", name, "--asset-id", &asset.id, "--confidence", "0.85", "--json"], true)?["changed"], 0);
    }
    let blocked = cli(&binary, &["agent-link", "tags", "--live", "--name", "造型:像素风", "--asset-id", &asset.id, "--confidence", "0.85", "--json"], true)?;
    assert_eq!(blocked["blockedByUser"], 1);
    let added = cli(&binary, &["agent-link", "tags", "--live", "--name", "造型:低多边形", "--asset-id", &asset.id, "--confidence", "0.85", "--json"], true)?;
    assert_eq!(added["changed"], 1);
    cli(&binary, &["agent-link", "log", "--live", "--message", "已追加 1 个标签，下一步同步资源卡", "--percent", "60", "--json"], true)?;
    let active = server.snapshot()?;
    let premature = cli(&binary, &["agent-link", "finish", "--live", "--summary", "尝试提前结束", "--json"], false)?;
    assert_eq!(premature["error_code"], "AgentLinkError");
    let synced = cli(&binary, &["agent-link", "sync-cards", "--live", "--asset-id", &asset.id, "--json"], true)?;
    assert_eq!(synced["partialFailure"], false);
    assert_eq!(synced["completed"].as_array().unwrap().len(), 1);
    assert_eq!(library.card_thumbnail(&asset.id)?.unwrap(), preview);
    let inspected = cli(&binary, &["agent-link", "inspect", "--live", "--asset-id", &asset.id, "--json"], true)?;
    assert_eq!(inspected["card"]["status"], "CardValid");
    cli(&binary, &["agent-link", "finish", "--live", "--summary", "检查 1 项，新增 1 个标签；保留手动标签与预览，尊重 1 项移除覆盖", "--json"], true)?;
    let finished = server.snapshot()?;
    assert_eq!(finished.status, "finished"); assert_eq!(finished.percent, Some(100));
    assert!(finished.library_revision > active.library_revision);
    let tags = library.list_asset_tags(&asset.id)?;
    assert!(tags.iter().any(|tag| tag.name == "造型:低多边形" && tag.source == "agent"));
    assert!(tags.iter().any(|tag| tag.name == "用途:参考模型" && tag.source == "user"));
    assert!(tags.iter().any(|tag| tag.name == "格式:PMX" && tag.source == "parser"));
    assert!(!tags.iter().any(|tag| tag.name == "造型:像素风"));
    server.new_session()?;
    cli(&binary, &["agent-link", "identify", "--live", "--name", "Codex", "--json"], true)?;
    let cancelled = server.cancel()?;
    let after_cancel = cli(&binary, &["agent-link", "log", "--live", "--message", "旧任务不应继续", "--json"], false)?;
    assert_eq!(after_cancel["error_code"], "AgentLinkError");
    let renewed = server.new_session()?;
    assert_ne!(renewed.session_id, cancelled.session_id);
    let report = json!({"synthetic":true,"source":"Core agentlink_probe with an actual CLI child process and software-rendered PMX preview",
        "roots":[root],"asset":library.inspect_asset(&asset.id)?,"tags":tags,
        "snapshots":{"waiting":waiting,"modelWaiting":model_waiting,"scopedWaiting":scoped_waiting,"active":active,"finished":finished,"cancelled":cancelled,"renewed":renewed},
        "checks":{"portableCliAndSkill":true,"liveCliIdentity":true,"scopedInventory":true,"manualAndParserTagsPreserved":true,"userRemovalHonored":true,"tagWrittenThroughCore":true,"finishRequiresSync":true,"previewUnchanged":true,"cardValid":true,"completionRefreshRevision":true,"cancelBlocksFurtherRequests":true,"newSessionRotatesIdentity":true}});
    assert!(!serde_json::to_string(&report)?.contains("\"token\""));
    fs::write(output.join("manifest.json"), serde_json::to_vec_pretty(&report)?)?;
    drop(server);
    assert!(!fixture.join("data/library.sqlite3.agentlink/session.json").exists());
    let disconnected = cli(&binary, &["agent-link", "inspect", "--live", "--json"], false)?;
    assert_eq!(disconnected["error_code"], "AgentLinkError");
    Ok(())
}
