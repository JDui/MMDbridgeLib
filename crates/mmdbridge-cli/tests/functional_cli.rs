use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mmdbridge_core::{AssetType, Library};
use serde_json::{Value, json};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const POSE: &str = "Vocaloid Pose Data file\n\nfixture.osm;\n1;\n\nBone0{center\n0.0,0.0,0.0;\n0.0,0.0,0.0,1.0;\n}\n";

struct Fixture {
    path: PathBuf,
    binary: PathBuf,
    assets: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mmdbridge-cli-e2e-{}-{}-{nonce}-中文_日本",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        let binary = path.join(format!("mmdbridge{}", std::env::consts::EXE_SUFFIX));
        fs::copy(env!("CARGO_BIN_EXE_mmdbridge"), &binary).unwrap();
        let assets = path.join("动作_日本");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("日本姿势.vpd"), POSE).unwrap();
        Self { path, binary, assets }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut child = Command::new(&self.binary)
            .args(args)
            .arg("--json")
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!("CLI timed out for {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            }
            thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }

    fn succeed(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn fail(&self, args: &[&str], code: &str) -> Value {
        let output = self.run(args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error_code"], code);
        assert!(error["recoverable"].is_boolean());
        error
    }

    fn seed_empty_model(&self) -> (String, String) {
        let mut bytes = b"PMX ".to_vec();
        bytes.extend_from_slice(&2.0_f32.to_le_bytes());
        bytes.extend_from_slice(&[8, 1, 0, 1, 1, 1, 1, 1, 1]);
        for _ in 0..13 {
            bytes.extend_from_slice(&0_i32.to_le_bytes());
        }
        fs::write(self.assets.join("空模型_日本.pmx"), bytes).unwrap();
        let library = Library::open(self.path.join("data/library.sqlite3")).unwrap();
        let root = library.add_root(AssetType::Model, self.assets.to_str().unwrap(), Some("模型")).unwrap();
        library.scan_root(&root.id).unwrap();
        let assets = library.list_assets(Some(AssetType::Model), None, 10).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].metadata["vertex_count"], 0);
        assert!(assets[0].statuses.iter().any(|status| status == "Ready"));
        (root.id, assets[0].id.clone())
    }

    fn seed_motion(&self) -> (String, String) {
        let library = Library::open(self.path.join("data/library.sqlite3")).unwrap();
        let root = library.add_root(AssetType::Motion, self.assets.to_str().unwrap(), Some("动作")).unwrap();
        library.scan_root(&root.id).unwrap();
        let assets = library.list_assets(Some(AssetType::Motion), None, 10).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].metadata["is_pose"], true);
        (root.id, assets[0].id.clone())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temporary = std::env::temp_dir();
        assert!(self.path.is_absolute() && self.path.starts_with(&temporary));
        assert!(self.path.file_name().unwrap().to_string_lossy().starts_with("mmdbridge-cli-e2e-"));
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn errors_have_structured_json_and_nonzero_exit_codes() {
    let fixture = Fixture::new();
    assert_eq!(fixture.succeed(&["roots", "list"]), json!([]));
    assert_eq!(fixture.succeed(&["jobs", "list"]), json!([]));
    fixture.fail(&["assets", "inspect", "missing-asset"], "AssetNotFound");
    fixture.fail(&["scan-status", "--root", "missing-root"], "RootNotFound");
    fixture.fail(&["jobs", "cancel", "missing-job"], "JobNotFound");
    fixture.fail(
        &["roots", "add", "--type", "invalid", "--path", fixture.assets.to_str().unwrap()],
        "InvalidAssetType",
    );
}

#[test]
fn unicode_scan_tags_favorites_and_root_removal_share_the_portable_database() {
    let fixture = Fixture::new();
    let root = fixture.succeed(&[
        "roots", "add", "--type", "motion", "--path", fixture.assets.to_str().unwrap(), "--name", "动作_日本",
    ]);
    let root_id = root["id"].as_str().unwrap();
    assert_eq!(fixture.succeed(&["roots", "list"])[0]["id"], root_id);
    let scan = fixture.succeed(&["scan", "--root", root_id]);
    assert_eq!(scan["status"], "Completed");
    assert_eq!(fixture.succeed(&["scan-status", "--root", root_id])["status"], "Completed");
    let assets = fixture.succeed(&["assets", "list", "--type", "motion", "--query", "日本"]);
    assert_eq!(assets.as_array().unwrap().len(), 1);
    let id = assets[0]["id"].as_str().unwrap();
    assert_eq!(fixture.succeed(&["assets", "inspect", id])["metadata"]["is_pose"], true);

    assert_eq!(fixture.succeed(&["tags", "add", id, "手动标签"])["changed"], true);
    fixture.succeed(&["tags", "remove", id, "手动标签"]);
    assert_eq!(
        fixture.succeed(&["tags", "add", id, "手动标签", "--source", "agent", "--confidence", "0.8"])["blockedByUser"],
        true,
    );
    let tags = fixture.succeed(&["tags", "list", id, "--include-overrides"]);
    assert!(tags["suppressedTags"].as_array().unwrap().contains(&json!("手动标签")));
    assert_eq!(fixture.succeed(&["favorites", "add", id])["favorite"], true);
    assert_eq!(fixture.succeed(&["favorites", "list"])[0]["id"], id);
    fixture.succeed(&["favorites", "remove", id]);
    assert_eq!(fixture.succeed(&["favorites", "list"]), json!([]));

    fixture.succeed(&["roots", "update", root_id, "--enabled", "false", "--recursive", "false"]);
    fixture.fail(&["scan", "--root", root_id], "RootDisabled");
    assert_eq!(fixture.succeed(&["roots", "remove", root_id])["removed"], true);
    assert!(fixture.assets.join("日本姿势.vpd").is_file());
    assert_eq!(fixture.succeed(&["assets", "list"]), json!([]));
    assert!(fixture.path.join("data/library.sqlite3").is_file());
}

#[test]
fn saved_filters_relations_and_invalid_settings_preserve_existing_state() {
    let fixture = Fixture::new();
    let (_, id) = fixture.seed_motion();
    fixture.succeed(&["tags", "add", &id, "筛选标签"]);
    fixture.succeed(&["favorites", "add", &id]);
    let expression = json!({
        "op":"and",
        "children":[
            {"op":"rule","field":"favorite","operator":"eq","value":true},
            {"op":"rule","field":"tag","operator":"eq","value":"筛选标签"}
        ]
    }).to_string();
    let filter = fixture.succeed(&["filters", "save", "--name", "收藏_日本", "--expression", &expression]);
    let filter_id = filter["id"].as_str().unwrap();
    assert_eq!(fixture.succeed(&["filters", "run", filter_id])[0]["id"], id);
    fixture.fail(&["filters", "save", "--name", "invalid", "--expression", r#"{"op":"and","children":[]}"#], "InvalidFilter");
    assert_eq!(fixture.succeed(&["filters", "list"]).as_array().unwrap().len(), 1);
    fixture.succeed(&["relations", "refresh"]);
    assert_eq!(fixture.succeed(&["relations", "list"]), json!([]));
    assert_eq!(fixture.succeed(&["settings", "motion-preview-model", "get"])["motion_preview_model"], Value::Null);
    fixture.fail(
        &["settings", "motion-preview-model", "set", fixture.assets.join("日本姿势.vpd").to_str().unwrap()],
        "ThumbnailRenderError",
    );
    assert_eq!(fixture.succeed(&["settings", "motion-preview-model", "get"])["motion_preview_model"], Value::Null);
    assert_eq!(fixture.succeed(&["filters", "remove", filter_id])["removed"], true);
}

#[test]
fn failed_worker_commands_fail_but_job_queries_still_succeed() {
    let fixture = Fixture::new();
    let (root_id, id) = fixture.seed_empty_model();
    let enqueue = fixture.run(&["thumbnail", "enqueue", &id]);
    assert!(!enqueue.status.success(), "failed thumbnail must not report a successful exit code");
    let job: Value = serde_json::from_slice(&enqueue.stdout).unwrap();
    assert_eq!(job["status"], "Failed");
    assert!(!fixture.succeed(&["jobs", "list"]).as_array().unwrap().is_empty());

    let batch = fixture.run(&["thumbnail", "batch", &id, &id]);
    assert!(!batch.status.success());
    let jobs: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(jobs.as_array().unwrap().len(), 1);
    assert_eq!(jobs[0]["status"], "Failed");

    let generate = fixture.run(&["cards", "generate", "--root", &root_id]);
    assert!(!generate.status.success());
    let summary: Value = serde_json::from_slice(&generate.stdout).unwrap();
    assert_eq!(summary["failed"], 1);

    let sync = fixture.run(&["cards", "sync-manifest", "--asset-id", &id]);
    assert!(!sync.status.success());
    let summary: Value = serde_json::from_slice(&sync.stdout).unwrap();
    assert_eq!(summary["partialFailure"], true);
    assert_eq!(fixture.succeed(&["jobs", "list"]).as_array().unwrap().len(), 3);
}
