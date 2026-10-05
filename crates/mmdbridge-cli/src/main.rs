use std::{io::Write, path::PathBuf, process::ExitCode, thread, time::Duration};

use clap::{Parser, Subcommand};
use mmdbridge_core::{AssetType, CoreError, FilterExpr, Library};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "mmdbridge",
    version,
    about = "MMDbridgeLib shared-core command line interface"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Roots {
        #[command(subcommand)]
        command: RootCommand,
    },
    Scan {
        #[arg(long)]
        root: String,
        #[arg(long)]
        queued: bool,
        #[arg(long)]
        full_check: bool,
    },
    /// Read persisted scan progress without enqueueing or resuming work.
    ScanStatus {
        #[arg(long)]
        root: Option<String>,
    },
    Assets {
        #[command(subcommand)]
        command: AssetCommand,
    },
    #[command(alias = "card")]
    Cards {
        #[command(subcommand)]
        command: CardCommand,
    },
    Tags {
        #[command(subcommand)]
        command: TagCommand,
    },
    Favorites {
        #[command(subcommand)]
        command: FavoriteCommand,
    },
    #[command(alias = "relation")]
    Relations {
        #[command(subcommand)]
        command: RelationCommand,
    },
    Filters {
        #[command(subcommand)]
        command: FilterCommand,
    },
    Settings {
        #[command(subcommand)]
        command: SettingCommand,
    },
    Thumbnail {
        #[command(subcommand)]
        command: ThumbnailCommand,
    },
    Jobs {
        #[command(subcommand)]
        command: JobCommand,
    },
}

#[derive(Debug, Subcommand)]
enum RootCommand {
    List,
    Add {
        #[arg(long = "type")]
        asset_type: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        name: Option<String>,
    },
    Update {
        root_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: Option<bool>,
        #[arg(long, action = clap::ArgAction::Set)]
        recursive: Option<bool>,
    },
    Remove {
        root_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum AssetCommand {
    List {
        #[arg(long = "type")]
        asset_type: Option<String>,
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Inspect {
        asset_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum CardCommand {
    Pending,
    Generate {
        #[arg(long)]
        root: String,
    },
    Thumbnail {
        asset_id: String,
        #[arg(long)]
        output: PathBuf,
    },
    Create {
        asset_id: String,
        #[arg(long)]
        preview: Option<PathBuf>,
    },
    Refresh {
        asset_id: String,
        #[arg(long)]
        preview: Option<PathBuf>,
    },
    SyncManifest {
        #[arg(long = "asset-id", required = true, num_args = 1..)]
        asset_ids: Vec<String>,
    },
    Verify {
        asset_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum TagCommand {
    AuditBatch {
        #[arg(long = "asset-id", required = true, num_args = 1..=100)]
        asset_ids: Vec<String>,
    },
    List {
        asset_id: String,
        #[arg(long)]
        include_overrides: bool,
    },
    Add {
        asset_id: String,
        name: String,
        #[arg(long, default_value = "user")]
        source: String,
        #[arg(long)]
        confidence: Option<f64>,
    },
    BatchAdd {
        #[arg(long)]
        name: String,
        #[arg(long = "asset-id", required = true, num_args = 1..)]
        asset_ids: Vec<String>,
        #[arg(long, default_value = "agent")]
        source: String,
        #[arg(long)]
        confidence: Option<f64>,
    },
    Remove {
        asset_id: String,
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum FavoriteCommand {
    List {
        #[arg(long, default_value_t = 500)]
        limit: usize,
        #[arg(long)]
        query: Option<String>,
    },
    Add {
        asset_id: String,
    },
    Remove {
        asset_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum RelationCommand {
    List {
        #[arg(long)]
        asset: Option<String>,
        #[arg(long = "type")]
        relation_type: Option<String>,
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    Refresh,
    Confirm {
        relation_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum FilterCommand {
    List,
    Save {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        expression: String,
    },
    Run {
        filter_id: String,
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    Remove {
        filter_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum JobCommand {
    List,
    Summary,
    Cancel { job_id: String },
    Retry { job_id: String },
}

#[derive(Debug, Subcommand)]
enum ThumbnailCommand {
    Enqueue {
        asset_id: String,
        #[arg(long, default_value_t = 0)]
        priority: i32,
    },
    Batch {
        #[arg(required = true)]
        asset_ids: Vec<String>,
        #[arg(long, default_value_t = 0)]
        priority: i32,
    },
}

#[derive(Debug, Subcommand)]
enum SettingCommand {
    MotionPreviewModel {
        #[command(subcommand)]
        command: MotionPreviewModelCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MotionPreviewModelCommand {
    Get,
    Set { path: PathBuf },
    Clear,
}

struct CommandOutput {
    value: serde_json::Value,
    failed: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(output) => {
            let value = output.value;
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".to_owned())
            );
            if output.failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let (asset_id, root_id, job_id) = match &error {
                CoreError::AssetNotFound(asset_id) => (Some(asset_id.as_str()), None, None),
                CoreError::RootNotFound(root_id) | CoreError::RootDisabled(root_id) => {
                    (None, Some(root_id.as_str()), None)
                }
                CoreError::JobNotFound(job_id) => (None, None, Some(job_id.as_str())),
                _ => (None, None, None),
            };
            let output = json!({"error_code":error_code(&error), "message":error.to_string(), "asset_id":asset_id, "root_id":root_id, "job_id":job_id, "source":null, "recoverable":error.is_recoverable()});
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{\"error_code\":\"Unknown\"}".to_owned())
            );
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<CommandOutput, CoreError> {
    let cli = Cli::parse();
    let _json_output = cli.json;
    let library = Library::open(Library::portable_database_path()?)?;
    let checks_job_outcome = matches!(
        &cli.command,
        Command::Thumbnail { .. }
            | Command::Cards {
                command: CardCommand::Generate { .. } | CardCommand::SyncManifest { .. }
            }
    );
    let value = match cli.command {
        Command::Roots {
            command: RootCommand::List,
        } => Ok(json!(library.list_roots()?)),
        Command::Roots {
            command:
                RootCommand::Add {
                    asset_type,
                    path,
                    name,
                },
        } => {
            let asset_type = AssetType::parse(&asset_type)
                .ok_or_else(|| CoreError::InvalidAssetType(asset_type))?;
            Ok(json!(library.add_root(
                asset_type,
                &path,
                name.as_deref()
            )?))
        }
        Command::Roots {
            command:
                RootCommand::Update {
                    root_id,
                    name,
                    enabled,
                    recursive,
                },
        } => Ok(json!(library.update_root(
            &root_id,
            enabled,
            recursive,
            name.as_deref()
        )?)),
        Command::Roots {
            command: RootCommand::Remove { root_id },
        } => Ok(json!({"removed":library.remove_root(&root_id)?})),
        Command::ScanStatus { root } => {
            let states = library.list_scan_states()?;
            match root {
                Some(root) => Ok(json!(states.into_iter().find(|state| state.root_id == root)
                    .ok_or_else(|| CoreError::RootNotFound(root))?)),
                None => Ok(json!(states)),
            }
        }
        Command::Scan { root, queued: _, full_check } => {
            let current = library.list_scan_states()?.into_iter()
                .find(|state| state.root_id == root);
            match current.as_ref().map(|state| state.status.as_str()) {
                Some("Paused") if !full_check => { library.continue_scan(&root)?; }
                Some("Paused") => {
                    return Err(CoreError::InvalidRoot("请先继续或停止已暂停的扫描，再开始完整检查".to_owned()));
                }
                Some("Pending" | "Pausing" | "Cancelling" | "Discovering" | "Indexing" | "Verifying" | "Relations") => {
                    return Err(CoreError::InvalidRoot("该目录已有扫描任务；请在桌面扫描队列中管理".to_owned()));
                }
                _ => {
                    if full_check { library.enqueue_full_check(&root)?; }
                    else { library.enqueue_scan(&root)?; }
                }
            }
            loop {
                let state = library.list_scan_states()?.into_iter()
                    .find(|state| state.root_id == root)
                    .ok_or_else(|| CoreError::RootNotFound(root.clone()))?;
                let scope = match state.scope.as_str() {
                    "full" => " · 完整发现",
                    "local" => " · 局部更新",
                    _ => "",
                };
                let full_check = if state.full_check { " · 深度检查" } else { "" };
                eprintln!("扫描 {}：{}{}{} · {}/{} 文件 · {:.1}%", root, state.status, scope, full_check,
                    state.files_processed, state.files_seen, state.progress * 100.0);
                match state.status.as_str() {
                    "Completed" => break Ok(json!(state)),
                    "Failed" => break Err(CoreError::InvalidRoot(state.error.unwrap_or_else(|| "扫描失败".to_owned()))),
                    "Cancelled" => break Err(CoreError::ScanCancelled),
                    "Paused" => break Err(CoreError::ScanPaused),
                    _ => thread::sleep(Duration::from_secs(5)),
                }
            }
        }
        Command::Assets {
            command:
                AssetCommand::List {
                    asset_type,
                    query,
                    limit,
                },
        } => {
            let asset_type = match asset_type {
                Some(value) => Some(
                    AssetType::parse(&value).ok_or_else(|| CoreError::InvalidAssetType(value))?,
                ),
                None => None,
            };
            Ok(json!(library.list_assets(
                asset_type,
                query.as_deref(),
                limit
            )?))
        }
        Command::Assets {
            command: AssetCommand::Inspect { asset_id },
        } => Ok(json!(library.inspect_asset(&asset_id)?)),
        Command::Cards {
            command: CardCommand::Pending,
        } => Ok(json!(library.pending_cards()?)),
        Command::Cards {
            command: CardCommand::Generate { root },
        } => {
            library.resume_thumbnail_jobs()?;
            let jobs = library.queue_pending_cards(&root)?;
            let total = jobs.len();
            loop {
                let mut completed = 0;
                let mut failed = 0;
                let mut cancelled = 0;
                for job_id in &jobs {
                    let job = library.wait_for_job(job_id, Duration::ZERO)?;
                    match job.get("status").and_then(serde_json::Value::as_str) {
                        Some("Completed") => completed += 1,
                        Some("Failed") => failed += 1,
                        Some("Cancelled") => cancelled += 1,
                        _ => {}
                    }
                }
                eprintln!("资源卡进度：{}/{total} 完成，{} 失败，{} 取消", completed, failed, cancelled);
                if completed + failed + cancelled == total {
                    break Ok(json!({"rootId":root,"total":total,"completed":completed,
                        "failed":failed,"cancelled":cancelled}));
                }
                thread::sleep(Duration::from_secs(10));
            }
        }
        Command::Cards {
            command: CardCommand::Thumbnail { asset_id, output },
        } => {
            let preview = library.card_thumbnail(&asset_id)?.ok_or_else(|| {
                CoreError::Card(format!("资源卡没有可读取的缩略图：{asset_id}"))
            })?;
            let mut file = std::fs::OpenOptions::new()
                .write(true).create_new(true).open(&output)?;
            file.write_all(&preview)?;
            Ok(json!({"assetId":asset_id,"output":output,"bytes":preview.len()}))
        }
        Command::Cards {
            command:
                CardCommand::Create { asset_id, preview } | CardCommand::Refresh { asset_id, preview },
        } => {
            let preview = preview.map(read_preview).transpose()?;
            let result = match preview.as_deref() {
                Some(preview) => library.create_card(&asset_id, Some(preview))?,
                None => library.create_card_with_thumbnail(&asset_id)?,
            };
            Ok(json!(result))
        }
        Command::Cards {
            command: CardCommand::SyncManifest { asset_ids },
        } => {
            let mut completed = Vec::new();
            let mut failed = Vec::new();
            for asset_id in asset_ids {
                let result = library.card_thumbnail(&asset_id).and_then(|preview| {
                    if preview.is_none() {
                        return Err(CoreError::Card(format!(
                            "资源卡没有当前可用的缩略图，无法仅同步 manifest：{asset_id}"
                        )));
                    }
                    library.create_card(&asset_id, None)
                });
                match result {
                    Ok(card) => completed.push(json!({"assetId":asset_id,"card":card})),
                    Err(error) => failed.push(json!({"assetId":asset_id,"error":error.to_string()})),
                }
            }
            Ok(json!({"total":completed.len() + failed.len(),"partialFailure":!failed.is_empty(),"completed":completed,"failed":failed}))
        }
        Command::Cards {
            command: CardCommand::Verify { asset_id },
        } => Ok(json!(library.verify_card(&asset_id)?)),
        Command::Tags {
            command: TagCommand::AuditBatch { asset_ids },
        } => {
            let mut records = Vec::with_capacity(asset_ids.len());
            for asset_id in asset_ids {
                records.push(json!({
                    "assetId": asset_id,
                    "card": library.verify_card(&asset_id)?,
                    "tags": library.list_asset_tags(&asset_id)?,
                    "suppressedTags": library.list_asset_tag_overrides(&asset_id)?
                }));
            }
            Ok(json!(records))
        }
        Command::Tags {
            command: TagCommand::List { asset_id, include_overrides },
        } => {
            let tags = library.list_asset_tags(&asset_id)?;
            if include_overrides {
                Ok(json!({"tags":tags,"suppressedTags":library.list_asset_tag_overrides(&asset_id)?}))
            } else {
                Ok(json!(tags))
            }
        }
        Command::Tags {
            command:
                TagCommand::Add {
                    asset_id,
                    name,
                    source,
                    confidence,
                },
        } => Ok(json!(
            library.add_asset_tag(&asset_id, &name, &source, confidence)?
        )),
        Command::Tags {
            command:
                TagCommand::BatchAdd {
                    name,
                    asset_ids,
                    source,
                    confidence,
                },
        } => Ok(json!(library.add_asset_tag_batch(
            &asset_ids,
            &name,
            &source,
            confidence
        )?)),
        Command::Tags {
            command: TagCommand::Remove { asset_id, name },
        } => Ok(json!(library.remove_asset_tag(&asset_id, &name)?)),
        Command::Favorites {
            command: FavoriteCommand::List { limit, query },
        } => Ok(json!(library.favorite_assets(query.as_deref(), limit)?)),
        Command::Favorites {
            command: FavoriteCommand::Add { asset_id },
        } => Ok(json!({"asset_id":asset_id,"favorite":library.set_favorite(&asset_id,true)?})),
        Command::Favorites {
            command: FavoriteCommand::Remove { asset_id },
        } => Ok(json!({"asset_id":asset_id,"favorite":library.set_favorite(&asset_id,false)?})),
        Command::Relations {
            command:
                RelationCommand::List {
                    asset,
                    relation_type,
                    limit,
                },
        } => Ok(json!(library.list_relations(
            asset.as_deref(),
            relation_type.as_deref(),
            limit
        )?)),
        Command::Relations {
            command: RelationCommand::Refresh,
        } => Ok(json!(library.rebuild_relations()?)),
        Command::Relations {
            command: RelationCommand::Confirm { relation_id },
        } => Ok(
            json!({"relation_id":relation_id,"confirmed":library.confirm_relation(&relation_id)?}),
        ),
        Command::Filters {
            command: FilterCommand::List,
        } => Ok(json!(library.list_saved_filters()?)),
        Command::Filters {
            command:
                FilterCommand::Save {
                    id,
                    name,
                    expression,
                },
        } => {
            let expression: FilterExpr = serde_json::from_str(&expression)?;
            Ok(json!(library.save_filter(
                id.as_deref(),
                &name,
                expression
            )?))
        }
        Command::Filters {
            command:
                FilterCommand::Run {
                    filter_id,
                    query,
                    limit,
                },
        } => Ok(json!(library.apply_saved_filter(
            &filter_id,
            query.as_deref(),
            limit
        )?)),
        Command::Filters {
            command: FilterCommand::Remove { filter_id },
        } => Ok(json!({"removed":library.remove_saved_filter(&filter_id)?})),
        Command::Settings {
            command:
                SettingCommand::MotionPreviewModel {
                    command: MotionPreviewModelCommand::Get,
                },
        } => Ok(json!({"motion_preview_model":library.motion_preview_model()?})),
        Command::Settings {
            command:
                SettingCommand::MotionPreviewModel {
                    command: MotionPreviewModelCommand::Set { path },
                },
        } => Ok(
            json!({"motion_preview_model":library.set_motion_preview_model(Some(&path.to_string_lossy()))?}),
        ),
        Command::Settings {
            command:
                SettingCommand::MotionPreviewModel {
                    command: MotionPreviewModelCommand::Clear,
                },
        } => Ok(json!({"motion_preview_model":library.set_motion_preview_model(None)?})),
        Command::Jobs {
            command: JobCommand::List,
        } => Ok(json!(library.list_jobs()?)),
        Command::Jobs {
            command: JobCommand::Summary,
        } => Ok(library.job_summary()?),
        Command::Jobs {
            command: JobCommand::Cancel { job_id },
        } => Ok(json!({"job_id":job_id,"cancelled":library.cancel_job(&job_id)?})),
        Command::Jobs {
            command: JobCommand::Retry { job_id },
        } => Ok(library.retry_job(&job_id)?),
        Command::Thumbnail {
            command: ThumbnailCommand::Enqueue { asset_id, priority },
        } => {
            let job = library.enqueue_thumbnail(&asset_id, priority)?;
            let job_id = job
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CoreError::ThumbnailQueue("无法获取新任务 ID".to_owned()))?;
            Ok(library.wait_for_job(job_id, std::time::Duration::from_secs(1800))?)
        }
        Command::Thumbnail {
            command:
                ThumbnailCommand::Batch {
                    asset_ids,
                    priority,
                },
        } => {
            let jobs = library.enqueue_thumbnails(&asset_ids, priority)?;
            let mut completed = Vec::with_capacity(jobs.len());
            for job in jobs {
                let job_id = job
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| CoreError::ThumbnailQueue("无法获取任务 ID".to_owned()))?;
                completed.push(library.wait_for_job(job_id, std::time::Duration::from_secs(1800))?);
            }
            Ok(json!(completed))
        }
    }?;
    let failed = checks_job_outcome && job_output_failed(&value);
    Ok(CommandOutput { value, failed })
}

fn job_output_failed(value: &serde_json::Value) -> bool {
    if value.get("partialFailure").and_then(serde_json::Value::as_bool) == Some(true)
        || value.get("failed").and_then(serde_json::Value::as_u64).is_some_and(|count| count > 0)
        || value.get("cancelled").and_then(serde_json::Value::as_u64).is_some_and(|count| count > 0)
    {
        return true;
    }
    if let Some(status) = value.get("status").and_then(serde_json::Value::as_str) {
        return status != "Completed";
    }
    value.as_array().is_some_and(|jobs| jobs.iter().any(job_output_failed))
}

fn read_preview(path: PathBuf) -> Result<Vec<u8>, CoreError> {
    let metadata = std::fs::metadata(&path)?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err(CoreError::Card("preview.webp 超过 32 MiB 上限".to_owned()));
    }
    Ok(std::fs::read(path)?)
}

fn error_code(error: &CoreError) -> &'static str {
    match error {
        CoreError::Database(_) => "DatabaseError",
        CoreError::UnsupportedDatabaseVersion { .. } => "UnsupportedDatabaseVersion",
        CoreError::Io(_) => "FilesystemError",
        CoreError::InvalidRoot(_) => "InvalidRoot",
        CoreError::RootNotFound(_) => "RootNotFound",
        CoreError::RootDisabled(_) => "RootDisabled",
        CoreError::ScanCancelled => "ScanCancelled",
        CoreError::ScanPaused => "ScanPaused",
        CoreError::StorageLimit(_) => "StorageLimit",
        CoreError::ModelPreview(_) => "ModelPreviewError",
        CoreError::ThumbnailRender(_) => "ThumbnailRenderError",
        CoreError::ThumbnailQueue(_) => "ThumbnailQueueError",
        CoreError::AssetOperation(_) => "AssetOperationError",
        CoreError::ThumbnailCancelled => "ThumbnailCancelled",
        CoreError::JobNotFound(_) => "JobNotFound",
        CoreError::AssetNotFound(_) => "AssetNotFound",
        CoreError::InvalidAssetType(_) => "InvalidAssetType",
        CoreError::UnsupportedAssetFormat(_) => "UnsupportedAssetFormat",
        CoreError::Card(_) => "CardError",
        CoreError::InvalidTag(_) => "InvalidTag",
        CoreError::InvalidFilter(_) => "InvalidFilter",
        CoreError::Json(_) => "SerializationError",
        CoreError::LockPoisoned => "CoreLockPoisoned",
    }
}
