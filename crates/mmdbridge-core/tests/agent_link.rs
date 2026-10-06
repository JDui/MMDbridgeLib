use std::{fs, path::PathBuf, thread, time::{Duration,Instant}};
use mmdbridge_core::{AgentLinkScope, AgentLinkServer, AssetType, Library, agent_link_request};
use serde_json::{Value,json};

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
            library.scan_root(&root.id).unwrap();
            let asset=library.list_assets(Some(AssetType::Model),None,10).unwrap().into_iter().find(|asset|asset.root_id==root.id).unwrap();
            let rgba=vec![190u8;1024*1024*4];
            let image=webp::Encoder::from_rgba(&rgba,1024,1024).encode(50.0);
            library.create_card(&asset.id,Some(&image)).unwrap();assets.push(asset.id);
        }
        let server=library.start_agent_link().unwrap();
        Self {directory,library:Some(library),server:Some(server),asset:assets[0].clone(),other:assets[1].clone()}
    }
    fn database(&self) -> PathBuf {self.directory.join("data/library.sqlite3")}
    fn rpc(&self,command:&str,payload:Value) -> Result<Value,mmdbridge_core::CoreError> {
        agent_link_request(&self.database(),command,payload,Duration::from_secs(5))
    }
    fn connect(&self) {self.rpc("agent-identify",json!({"name":"Codex"})).unwrap();}
}
impl Drop for Fixture {
    fn drop(&mut self) {self.server.take();self.library.take();let _=fs::remove_dir_all(&self.directory);}
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
