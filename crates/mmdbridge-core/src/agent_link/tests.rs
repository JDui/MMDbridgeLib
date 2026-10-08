use std::{fs, path::PathBuf, thread, time::{Duration,Instant}};
use crate::{AgentLinkScope, AgentLinkServer, AssetType, Library, agent_link_request, CoreError};
use serde_json::{Value,json};

#[path = "../../tests/fixtures/mod.rs"]
mod fixtures;

struct Fixture { directory:PathBuf, library:Option<Library>, server:Option<AgentLinkServer>, asset:String, other:String }
impl Fixture {
    fn new() -> Self {
        let directory=std::env::temp_dir().join(format!("mbl-agentlink-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(directory.join("data")).unwrap();
        let library=Library::open(directory.join("data/library.sqlite3")).unwrap();
        let mut assets=Vec::new();
        for label in ["角色目录","另一个目录"] {
            let path=directory.join(label);fs::create_dir(&path).unwrap();
            fs::write(path.join("模型.pmx"),fixtures::pmx(false,2,0,false)).unwrap();
            fs::create_dir(path.join("貼図")).unwrap();
            image::RgbaImage::from_pixel(4,4,image::Rgba([180,190,210,255])).save(path.join("貼図/tex.png")).unwrap();
            let root=library.add_root(AssetType::Model,path.to_str().unwrap(),Some(label)).unwrap();
            assert_eq!(library.scan_root(&root.id).unwrap().parse_failures,0);
            let asset=library.list_assets(Some(AssetType::Model),None,10).unwrap().into_iter().find(|asset|asset.root_id==root.id).unwrap();
            let rgba=vec![190u8;1024*1024*4];
            let image=webp::Encoder::from_rgba(&rgba,1024,1024).encode(50.0);
            // Protocol fixtures exercise current-preview checks without requiring a GPU.
            // The separate agentlink_probe renders a real preview and runs the CLI.
            let report=crate::thumbnail::ThumbnailRenderReport {
                renderer_version:crate::thumbnail::RENDERER_VERSION.to_owned(),
                preview_settings_version:crate::thumbnail::PREVIEW_SETTINGS_VERSION.to_owned(),
                adapter:"Synthetic protocol fixture; no GPU render".to_owned(),front_axis:"-Z".to_owned(),
                width:1024,height:1024,format:"webp".to_owned(),quality:50,antialiasing_samples:1,
                preview_frame:None,vertex_count:3,triangle_count:1,material_count:1,texture_count:1,
                diagnostics:Vec::new(),subject_palette:None,
            };
            crate::cards::create(&library,&asset.id,Some(&image),Some(&report)).unwrap();assets.push(asset.id);
        }
        let server=library.start_agent_link().unwrap();
        Self {directory,library:Some(library),server:Some(server),asset:assets[0].clone(),other:assets[1].clone()}
    }
    fn database(&self) -> PathBuf {self.directory.join("data/library.sqlite3")}
    fn rpc(&self,command:&str,payload:Value) -> Result<Value,CoreError> {
        let session=self.server.as_ref().and_then(|server|server.snapshot().ok()).map(|snapshot|snapshot.session_id).unwrap_or_default();
        agent_link_request(&self.database(),&session,command,payload,Duration::from_secs(5))
    }
    fn connect(&self) {self.rpc("agent-identify",json!({"name":"Codex"})).unwrap();}
}
impl Drop for Fixture {
    fn drop(&mut self) {self.server.take();self.library.take();let _=fs::remove_dir_all(&self.directory);}
}

#[test]
fn failed_session_file_writes_keep_the_previous_session_available_for_retry() {
    let mut f=Fixture::new();
    let server=f.server.as_mut().unwrap();
    // Freeze only this isolated fixture's workers while simulating blocked file writes.
    server.stopped.store(true,super::Ordering::Release);
    for worker in server.workers.drain(..) { worker.join().unwrap(); }
    server.stopped.store(false,super::Ordering::Release);
    for name in ["owner.lock","session.json"] {
        let before=server.snapshot().unwrap();
        let target=server.directory.join(name);let backup=server.directory.join(format!("{name}.backup"));
        fs::rename(&target,&backup).unwrap();fs::create_dir(&target).unwrap();
        assert!(server.new_session().is_err());
        assert_eq!(server.auth.lock().unwrap().session_id,before.session_id);
        assert_eq!(server.snapshot().unwrap().status,before.status);
        fs::remove_dir(&target).unwrap();fs::rename(&backup,&target).unwrap();
        let restored=super::session_at(&server.directory,&server.database).unwrap();
        assert_eq!(restored.session_id,before.session_id);
        assert_eq!(restored.token,server.auth.lock().unwrap().token);
        let retried=server.new_session().unwrap();
        assert_ne!(retried.session_id,before.session_id);assert_eq!(retried.status,"waiting");
        assert_eq!(super::session_at(&server.directory,&server.database).unwrap().session_id,retried.session_id);
    }
}

#[test]
fn live_session_identifies_logs_and_requires_synced_cards_before_finish() {
    let f=Fixture::new();let library=f.library.as_ref().unwrap();
    assert!(f.rpc("agent-inspect",json!({})).is_err());f.connect();
    assert!(f.rpc("agent-identify",json!({"name":"Other"})).is_err());
    f.rpc("agent-log",json!({"message":"读取模型与已有标签","percent":25})).unwrap();
    f.rpc("agent-identify",json!({"name":"Codex"})).unwrap();
    assert_eq!(f.server.as_ref().unwrap().snapshot().unwrap().percent,Some(25));
    assert!(f.rpc("agent-log",json!({"message":"错误进度","percent":true})).is_err());
    let inspected=f.rpc("agent-inspect",json!({"assetId":f.asset})).unwrap();
    assert_eq!(inspected["card"]["status"],"CardValid");
    let before=library.card_thumbnail(&f.asset).unwrap().unwrap();
    let tags=f.rpc("agent-tags",json!({"assetIds":[f.asset],"name":"造型:低多边形","confidence":0.85})).unwrap();
    assert_eq!(tags["changed"],1);
    assert!(f.rpc("agent-finish",json!({"summary":"检查完成"})).is_err());
    let sync=f.rpc("agent-sync-cards",json!({"assetIds":[f.asset]})).unwrap();
    assert_eq!(sync["partialFailure"],false);
    assert_eq!(library.card_thumbnail(&f.asset).unwrap().unwrap(),before);
    f.rpc("agent-finish",json!({"summary":"完成 1 项，保留当前预览"})).unwrap();
    let state=f.server.as_ref().unwrap().snapshot().unwrap();
    assert_eq!(state.status,"finished");assert_eq!(state.percent,Some(100));
    assert!(state.events.iter().any(|event|event.operation=="agent-tags"));
    assert!(!serde_json::to_string(&state).unwrap().contains("\"token\""));
}

#[test]
fn live_tags_preserve_manual_parser_agent_assignments_and_user_removals() {
    let f=Fixture::new();let library=f.library.as_ref().unwrap();
    library.add_asset_tag(&f.asset,"服装:裙装","user",None).unwrap();
    library.add_asset_tag(&f.asset,"造型:二次元","agent",Some(0.75)).unwrap();
    library.remove_asset_tag(&f.asset,"穿搭:校园").unwrap();
    library.create_card(&f.asset,None).unwrap();f.connect();
    for name in ["服装:裙装","格式:PMX","造型:二次元"] {
        let value=f.rpc("agent-tags",json!({"assetIds":[f.asset],"name":name,"confidence":0.9})).unwrap();
        assert_eq!(value["changed"],0);
    }
    let blocked=f.rpc("agent-tags",json!({"assetIds":[f.asset],"name":"穿搭:校园","confidence":0.85})).unwrap();
    assert_eq!(blocked["blockedByUser"],1);
    let tags=library.list_asset_tags(&f.asset).unwrap();
    assert!(tags.iter().any(|tag|tag.name=="服装:裙装" && tag.source=="user"));
    assert!(tags.iter().any(|tag|tag.name=="格式:PMX" && tag.source=="parser"));
    assert!(tags.iter().any(|tag|tag.name=="造型:二次元" && tag.confidence==Some(0.75)));
    assert!(!tags.iter().any(|tag|tag.name=="穿搭:校园"));
}

#[test]
fn unverified_preview_is_skipped_without_writes_or_render_jobs() {
    let f=Fixture::new();let library=f.library.as_ref().unwrap();
    let rgba=vec![160u8;1024*1024*4];
    let preview=webp::Encoder::from_rgba(&rgba,1024,1024).encode(50.0);
    library.create_card(&f.asset,Some(&preview)).unwrap();
    let jobs=library.list_jobs().unwrap();f.connect();
    assert_eq!(f.rpc("agent-inspect",json!({"assetId":f.asset})).unwrap()["card"]["status"],"CardStale");
    assert!(f.rpc("agent-tags",json!({"assetIds":[f.asset],"name":"服装:制服","confidence":0.85})).is_err());
    assert!(!library.list_asset_tags(&f.asset).unwrap().iter().any(|tag|tag.name=="服装:制服"));
    assert_eq!(library.list_jobs().unwrap(),jobs);
    f.rpc("agent-finish",json!({"summary":"跳过 1 项：预览未验证，未添加标签"})).unwrap();
}

#[test]
fn old_prompt_cannot_join_or_write_to_a_replacement_session() {
    let f=Fixture::new();let server=f.server.as_ref().unwrap();f.connect();
    let old=server.snapshot().unwrap().session_id;
    server.cancel().unwrap();server.new_session().unwrap();f.connect();
    for (command,payload) in [
        ("agent-identify",json!({"name":"Codex"})),
        ("agent-tags",json!({"assetIds":[f.asset],"name":"服装:制服","confidence":0.85})),
    ] {
        assert!(agent_link_request(&f.database(),&old,command,payload,Duration::from_secs(5)).is_err());
    }
    assert!(!f.library.as_ref().unwrap().list_asset_tags(&f.asset).unwrap().iter().any(|tag|tag.name=="服装:制服"));
    assert_eq!(server.snapshot().unwrap().status,"active");
}

#[test]
fn live_scope_blocks_outside_assets_and_cancel_requires_a_new_session() {
    let f=Fixture::new();let library=f.library.as_ref().unwrap();let server=f.server.as_ref().unwrap();
    let root=library.inspect_asset(&f.asset).unwrap().root_id;
    server.set_scope(AgentLinkScope {root_id:Some(root),asset_type:Some(AssetType::Model)}).unwrap();f.connect();
    assert!(server.set_scope(AgentLinkScope::default()).is_err());
    assert!(f.rpc("agent-inspect",json!({"assetId":f.other})).is_err());
    assert!(f.rpc("agent-tags",json!({"assetIds":[f.asset,f.other],"name":"造型:Q版","confidence":0.85})).is_err());
    assert!(!library.list_asset_tags(&f.asset).unwrap().iter().any(|tag|tag.name=="造型:Q版"));
    assert!(f.rpc("agent-tags",json!({"assetIds":[f.asset],"name":"裙型:百褶裙","confidence":0.75})).is_err());
    server.cancel().unwrap();assert!(f.rpc("agent-identify",json!({"name":"Codex"})).is_err());
    server.new_session().unwrap();f.connect();assert_eq!(server.snapshot().unwrap().status,"active");
}

#[test]
fn invalid_packets_do_not_disconnect_the_bridge_and_duplicate_owners_are_rejected() {
    let f=Fixture::new();let server=f.server.as_ref().unwrap();
    assert!(f.library.as_ref().unwrap().start_agent_link().is_err());
    let root=f.directory.join("data/library.sqlite3.agentlink");
    let session:Value=serde_json::from_slice(&fs::read(root.join("session.json")).unwrap()).unwrap();
    let id=uuid::Uuid::new_v4().to_string();let name=format!("{}-{id}.json",session["sessionId"].as_str().unwrap());
    fs::write(root.join("requests").join(&name),serde_json::to_vec(&json!({"id":id,"sessionId":session["sessionId"],"token":"invalid","command":"agent-identify","payload":{"name":"Bad"}})).unwrap()).unwrap();
    let deadline=Instant::now()+Duration::from_secs(5);
    while !root.join("responses").join(&name).is_file() {assert!(Instant::now()<deadline);thread::sleep(Duration::from_millis(20));}
    let response:Value=serde_json::from_slice(&fs::read(root.join("responses").join(&name)).unwrap()).unwrap();
    assert_eq!(response["ok"],false);assert_eq!(server.snapshot().unwrap().status,"waiting");
    f.connect();assert!(f.rpc("asset-delete",json!({"assetId":f.asset})).is_err());
    assert!(PathBuf::from(f.library.as_ref().unwrap().inspect_asset(&f.asset).unwrap().primary_source).is_file());
}

#[test]
fn closing_cleans_the_owned_session_and_history_does_not_replay_control() {
    let mut f=Fixture::new();f.connect();f.rpc("agent-log",json!({"message":"日本語与中文记录","percent":60})).unwrap();
    f.server.take();assert!(!f.directory.join("data/library.sqlite3.agentlink/session.json").exists());
    assert!(f.rpc("agent-inspect",json!({})).is_err());
    let server=f.library.as_ref().unwrap().start_agent_link().unwrap();
    let snapshot=server.snapshot().unwrap();assert_eq!(snapshot.status,"waiting");assert_eq!(snapshot.agent_name,"");
    assert!(snapshot.events.iter().any(|event|event.message=="日本語与中文记录"));
    f.server=Some(server);
}
