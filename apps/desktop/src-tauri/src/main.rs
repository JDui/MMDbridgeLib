#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod file_watcher;

use mmdbridge_core::{
    Asset, AssetCursor, AssetOperationJournalEntry, AssetOperationPlan, AssetPage,
    AssetRelation, AssetTag, AssetType, CoreError, FilterExpr, Library, RelationRefreshReport,
    Root, SavedFilter, ScanState, TagMutation,
    ThumbnailConcurrencySettings, AutoTagSettings,
};
use serde::Serialize;
use tauri::{Manager, State, ipc::Response};

struct CoreState(Library, std::sync::atomic::AtomicBool);

#[derive(Clone, Serialize)]
struct StartupStatus {
    phase: String,
    detail: String,
    step: u8,
    completed: Option<usize>,
    total: Option<usize>,
    elapsed_ms: u64,
    phase_elapsed_ms: u64,
    idle_ms: u64,
    #[serde(skip_serializing)]
    phase_started_ms: u64,
    #[serde(skip_serializing)]
    updated_ms: u64,
    ready: bool,
    error: Option<String>,
}
struct StartupState {
    status: std::sync::Arc<std::sync::Mutex<StartupStatus>>,
    started: std::time::Instant,
}

#[tauri::command]
fn startup_status(state: State<'_, StartupState>) -> Result<StartupStatus, ApiError> {
    state.status.lock().map(|status| {
        let mut snapshot = status.clone();
        snapshot.elapsed_ms = state.started.elapsed().as_millis() as u64;
        snapshot.phase_elapsed_ms = snapshot.elapsed_ms.saturating_sub(status.phase_started_ms);
        snapshot.idle_ms = snapshot.elapsed_ms.saturating_sub(status.updated_ms);
        snapshot
    }).map_err(|_| CoreError::LockPoisoned.into())
}

#[tauri::command]
fn library_background_start(app: tauri::AppHandle, state: State<'_, CoreState>) {
    if state.1.swap(true, std::sync::atomic::Ordering::AcqRel) { return; }
    let library = state.0.clone();
    std::thread::spawn(move || {
        if let Err(error) = library.resume_scan_jobs() { eprintln!("扫描恢复失败：{error}"); }
        if let Err(error) = library.resume_thumbnail_jobs() { eprintln!("缩略图恢复失败：{error}"); }
        if let Err(error) = file_watcher::start(app, library) { eprintln!("文件监视器启动失败：{error}"); }
    });
}

async fn read_core<T: Send + 'static>(
    library: Library,
    action: impl FnOnce(Library) -> Result<T, CoreError> + Send + 'static,
) -> Result<T, ApiError> {
    tauri::async_runtime::spawn_blocking(move || action(library))
        .await
        .map_err(|error| ApiError::from(CoreError::Io(std::io::Error::other(error.to_string()))))?
        .map_err(Into::into)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiError {
    error_code: &'static str,
    message: String,
    asset_id: Option<String>,
    root_id: Option<String>,
    job_id: Option<String>,
    source: Option<String>,
    recoverable: bool,
}

impl From<CoreError> for ApiError {
    fn from(error: CoreError) -> Self {
        let recoverable = error.is_recoverable();
        let (asset_id, root_id, job_id) = match &error {
            CoreError::AssetNotFound(asset_id) => (Some(asset_id.clone()), None, None),
            CoreError::RootNotFound(root_id) | CoreError::RootDisabled(root_id) => {
                (None, Some(root_id.clone()), None)
            }
            CoreError::JobNotFound(job_id) => (None, None, Some(job_id.clone())),
            _ => (None, None, None),
        };
        let error_code = match &error {
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
        };
        Self {
            error_code,
            message: error.to_string(),
            asset_id,
            root_id,
            job_id,
            source: None,
            recoverable,
        }
    }
}

#[tauri::command]
async fn roots_list(state: State<'_, CoreState>) -> Result<Vec<Root>, ApiError> {
    read_core(state.0.clone(), |library| library.list_roots()).await
}

#[tauri::command]
async fn asset_counts(state: State<'_, CoreState>) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), |library| library.asset_counts()).await
}

#[tauri::command]
async fn asset_directories(state: State<'_, CoreState>, root_id: String) -> Result<Vec<serde_json::Value>, ApiError> {
    read_core(state.0.clone(), move |library| library.asset_directories(&root_id)).await
}

#[tauri::command]
async fn asset_directory_page(
    state: State<'_, CoreState>,
    root_id: String,
    path: Option<String>,
    recursive_scope: Option<bool>,
) -> Result<mmdbridge_core::DirectoryPage, ApiError> {
    read_core(state.0.clone(), move |library| {
        library.directory_page(&root_id, path.as_deref(), recursive_scope.unwrap_or(true))
    }).await
}

#[tauri::command]
fn root_add(
    state: State<'_, CoreState>,
    asset_type: String,
    path: String,
    name: Option<String>,
    scan_recursive: Option<bool>,
) -> Result<Root, ApiError> {
    let kind =
        AssetType::parse(&asset_type).ok_or_else(|| CoreError::InvalidAssetType(asset_type))?;
    state
        .0
        .add_root_with_recursive(kind, &path, name.as_deref(), scan_recursive.unwrap_or(true))
        .map_err(Into::into)
}

#[tauri::command]
fn root_remove(state: State<'_, CoreState>, root_id: String) -> Result<bool, ApiError> {
    state.0.remove_root(&root_id).map_err(Into::into)
}

#[tauri::command]
fn root_update(
    state: State<'_, CoreState>,
    root_id: String,
    enabled: Option<bool>,
    scan_recursive: Option<bool>,
    display_name: Option<String>,
) -> Result<Root, ApiError> {
    state
        .0
        .update_root(&root_id, enabled, scan_recursive, display_name.as_deref())
        .map_err(Into::into)
}

#[tauri::command]
async fn scan_enqueue(state: State<'_, CoreState>, root_id: String) -> Result<ScanState, ApiError> {
    read_core(state.0.clone(), move |library| library.enqueue_scan(&root_id)).await
}

#[tauri::command]
async fn scan_full_check(state: State<'_, CoreState>, root_id: String) -> Result<ScanState, ApiError> {
    read_core(state.0.clone(), move |library| library.enqueue_full_check(&root_id)).await
}

#[tauri::command]
async fn scan_states(state: State<'_, CoreState>) -> Result<Vec<ScanState>, ApiError> {
    read_core(state.0.clone(), |library| library.list_scan_states()).await
}

#[tauri::command]
async fn scan_cancel(state: State<'_, CoreState>, root_id: String) -> Result<bool, ApiError> {
    read_core(state.0.clone(), move |library| library.cancel_scan(&root_id)).await
}

#[tauri::command]
async fn scan_pause(state: State<'_, CoreState>, root_id: String) -> Result<bool, ApiError> {
    read_core(state.0.clone(), move |library| library.pause_scan(&root_id)).await
}

#[tauri::command]
async fn scan_continue(state: State<'_, CoreState>, root_id: String) -> Result<ScanState, ApiError> {
    read_core(state.0.clone(), move |library| library.continue_scan(&root_id)).await
}

#[tauri::command]
async fn scan_move(state: State<'_, CoreState>, root_id: String, direction: i32) -> Result<bool, ApiError> {
    read_core(state.0.clone(), move |library| library.move_pending_scan(&root_id, direction)).await
}

#[tauri::command]
async fn model_preview(state: State<'_, CoreState>, asset_id: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| library.model_preview(&asset_id))
        .await
        .map(Response::new)
}

#[tauri::command]
async fn motion_preview_frame(state: State<'_, CoreState>, asset_id: String, frame: u32) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), move |library| library.motion_preview_frame(&asset_id, frame)).await
}

#[tauri::command]
async fn model_preview_file(state: State<'_, CoreState>, path: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| library.model_preview_file(std::path::Path::new(&path)))
        .await
        .map(Response::new)
}

#[tauri::command]
async fn scene_preview(state: State<'_, CoreState>, asset_id: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| library.scene_preview(&asset_id)).await.map(Response::new)
}

#[tauri::command]
async fn scene_preview_texture(state: State<'_, CoreState>, asset_id: String, texture_path: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| library.scene_preview_texture(&asset_id, &texture_path))
        .await.map(|texture| {
            let Some((png, alpha_mode)) = texture else { return Response::new(Vec::new()); };
            let mut payload = Vec::with_capacity(png.len() + 1);
            payload.push(alpha_mode);
            payload.extend_from_slice(&png);
            Response::new(payload)
        })
}

#[tauri::command]
async fn model_preview_texture_file(state: State<'_, CoreState>, model_path: String, texture_path: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| {
        library.model_preview_texture_file(std::path::Path::new(&model_path), &texture_path)
    }).await.map(|texture| {
        let Some((png, alpha_mode)) = texture else { return Response::new(Vec::new()); };
        let mut payload = Vec::with_capacity(png.len() + 1);
        payload.push(alpha_mode);
        payload.extend_from_slice(&png);
        Response::new(payload)
    })
}

#[tauri::command]
fn motion_preview_model_get(state: State<'_, CoreState>) -> Result<Option<String>, ApiError> {
    state.0.motion_preview_model().map_err(Into::into)
}

#[tauri::command]
fn motion_preview_model_set(
    state: State<'_, CoreState>,
    path: Option<String>,
) -> Result<Option<String>, ApiError> {
    state
        .0
        .set_motion_preview_model(path.as_deref())
        .map_err(Into::into)
}

#[tauri::command]
fn thumbnail_concurrency_get(
    state: State<'_, CoreState>,
) -> Result<ThumbnailConcurrencySettings, ApiError> {
    state.0.thumbnail_concurrency().map_err(Into::into)
}

#[tauri::command]
fn auto_tag_settings_get(state: State<'_, CoreState>) -> Result<AutoTagSettings, ApiError> {
    state.0.auto_tag_settings().map_err(Into::into)
}

#[tauri::command]
fn auto_tag_settings_set(state: State<'_, CoreState>, settings: AutoTagSettings) -> Result<AutoTagSettings, ApiError> {
    state.0.set_auto_tag_settings(&settings).map_err(Into::into)
}

#[tauri::command]
fn thumbnail_concurrency_set(
    state: State<'_, CoreState>,
    settings: ThumbnailConcurrencySettings,
) -> Result<ThumbnailConcurrencySettings, ApiError> {
    state
        .0
        .set_thumbnail_concurrency(&settings)
        .map_err(Into::into)
}

#[tauri::command]
fn asset_operation_plan(
    state: State<'_, CoreState>,
    operation: String,
    asset_ids: Vec<String>,
    destination_parent: Option<String>,
    new_name: Option<String>,
) -> Result<AssetOperationPlan, ApiError> {
    state
        .0
        .plan_asset_operation(
            &operation,
            &asset_ids,
            destination_parent.as_deref(),
            new_name.as_deref(),
        )
        .map_err(Into::into)
}

#[tauri::command]
fn asset_operation_execute(
    state: State<'_, CoreState>,
    plan: AssetOperationPlan,
) -> Result<AssetOperationJournalEntry, ApiError> {
    state.0.execute_asset_operation(&plan).map_err(Into::into)
}

#[tauri::command]
fn operation_journal_list(
    state: State<'_, CoreState>,
    limit: Option<usize>,
) -> Result<Vec<AssetOperationJournalEntry>, ApiError> {
    state
        .0
        .list_operation_journal(limit.unwrap_or(100))
        .map_err(Into::into)
}

#[tauri::command]
fn operation_journal_resolve(
    state: State<'_, CoreState>,
    operation_id: String,
) -> Result<bool, ApiError> {
    state
        .0
        .resolve_operation_journal(&operation_id)
        .map_err(Into::into)
}

#[tauri::command]
fn assets_list(
    state: State<'_, CoreState>,
    asset_type: Option<String>,
    query: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Asset>, ApiError> {
    let kind = match asset_type {
        Some(value) => {
            Some(AssetType::parse(&value).ok_or_else(|| CoreError::InvalidAssetType(value))?)
        }
        None => None,
    };
    state
        .0
        .list_assets(kind, query.as_deref(), limit.unwrap_or(300))
        .map_err(Into::into)
}

#[tauri::command]
async fn assets_page(
    state: State<'_, CoreState>,
    asset_type: Option<String>,
    motion_format: Option<String>,
    directory_path: Option<String>,
    recursive_scope: Option<bool>,
    query: Option<String>,
    root_id: Option<String>,
    favorites_only: Option<bool>,
    filter_id: Option<String>,
    expression: Option<FilterExpr>,
    cursor: Option<AssetCursor>,
    limit: Option<usize>,
) -> Result<AssetPage, ApiError> {
    let page_size = limit.unwrap_or(500);
    let kind = match asset_type {
        Some(value) => {
            Some(AssetType::parse(&value).ok_or_else(|| CoreError::InvalidAssetType(value))?)
        }
        None => None,
    };
    let motion_format = match motion_format.as_deref() {
        None => None,
        Some("vmd" | "vpd") if kind == Some(AssetType::Motion) => motion_format,
        Some(value) => return Err(ApiError::from(CoreError::InvalidAssetType(value.to_owned()))),
    };
    read_core(state.0.clone(), move |library| {
        library.list_asset_page_with_filters(kind, query.as_deref(), root_id.as_deref(), favorites_only.unwrap_or(false), cursor.as_ref(), page_size, motion_format.as_deref(), directory_path.as_deref(), recursive_scope.unwrap_or(true), filter_id.as_deref(), expression)
    }).await
}

#[tauri::command]
async fn asset_inspect(state: State<'_, CoreState>, asset_id: String) -> Result<Asset, ApiError> {
    read_core(state.0.clone(), move |library| library.inspect_asset(&asset_id)).await
}

#[tauri::command]
fn asset_reveal(state: State<'_, CoreState>, asset_id: String) -> Result<(), ApiError> {
    let asset = state.0.inspect_asset(&asset_id).map_err(ApiError::from)?;
    #[cfg(target_os = "windows")]
    {
        let source = explorer_compatible_path(&asset.primary_source);
        let mut selection = std::ffi::OsString::from("/select,");
        selection.push(source.as_os_str());
        std::process::Command::new("explorer.exe")
            .arg(selection)
            .spawn()
            .map(|_| ())
            .map_err(|error| ApiError::from(CoreError::Io(error)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = asset;
        Err(ApiError::from(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "在此平台无法打开 Windows 资源管理器",
        ))))
    }
}

#[tauri::command]
fn asset_open_directory(state: State<'_, CoreState>, asset_id: String) -> Result<(), ApiError> {
    let asset = state.0.inspect_asset(&asset_id).map_err(ApiError::from)?;
    #[cfg(target_os = "windows")]
    {
        let directory = explorer_compatible_path(&asset.asset_directory);
        std::process::Command::new("explorer.exe")
            .arg(directory)
            .spawn()
            .map(|_| ())
            .map_err(|error| ApiError::from(CoreError::Io(error)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = asset;
        Err(ApiError::from(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "在此平台无法打开 Windows 资源管理器",
        ))))
    }
}

#[cfg(target_os = "windows")]
fn explorer_compatible_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        std::path::PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        std::path::PathBuf::from(rest)
    } else {
        std::path::PathBuf::from(path)
    }
}

#[tauri::command]
fn cards_pending(state: State<'_, CoreState>) -> Result<Vec<Asset>, ApiError> {
    state.0.pending_cards().map_err(Into::into)
}

#[tauri::command]
fn card_create(
    state: State<'_, CoreState>,
    asset_id: String,
) -> Result<serde_json::Value, ApiError> {
    let asset = state.0.inspect_asset(&asset_id).map_err(ApiError::from)?;
    let extension = std::path::Path::new(&asset.primary_source)
        .extension()
        .and_then(|extension| extension.to_str());
    if extension.is_some_and(|extension| asset.asset_type.supports_thumbnail_extension(extension)) {
        state.0.enqueue_thumbnail(&asset_id, 0).map_err(Into::into)
    } else {
        state
            .0
            .create_card(&asset_id, None)
            .and_then(|card| serde_json::to_value(card).map_err(Into::into))
            .map_err(Into::into)
    }
}

#[tauri::command]
fn thumbnail_enqueue(
    state: State<'_, CoreState>,
    asset_id: String,
    priority: Option<i32>,
) -> Result<serde_json::Value, ApiError> {
    state
        .0
        .enqueue_thumbnail(&asset_id, priority.unwrap_or(0))
        .map_err(Into::into)
}

#[tauri::command]
fn thumbnail_batch(
    state: State<'_, CoreState>,
    asset_ids: Vec<String>,
    priority: Option<i32>,
) -> Result<Vec<serde_json::Value>, ApiError> {
    state
        .0
        .enqueue_thumbnails(&asset_ids, priority.unwrap_or(0))
        .map_err(Into::into)
}

#[tauri::command]
async fn card_thumbnail(state: State<'_, CoreState>, asset_id: String) -> Result<Response, ApiError> {
    read_core(state.0.clone(), move |library| library.card_thumbnail(&asset_id))
        .await?
        .map(Response::new)
        .ok_or_else(|| CoreError::Card("资源卡中没有有效缩略图".to_owned()).into())
}

#[tauri::command]
fn card_verify(
    state: State<'_, CoreState>,
    asset_id: String,
) -> Result<mmdbridge_core::CardValidation, ApiError> {
    state.0.verify_card(&asset_id).map_err(Into::into)
}

#[tauri::command]
async fn cards_queue_root(state: State<'_, CoreState>, root_id: String) -> Result<usize, ApiError> {
    let library = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || library.queue_pending_cards(&root_id).map(|jobs| jobs.len()))
        .await
        .map_err(|error| CoreError::ThumbnailQueue(error.to_string()))?
        .map_err(Into::into)
}

#[tauri::command]
async fn asset_tags(state: State<'_, CoreState>, asset_id: String) -> Result<Vec<AssetTag>, ApiError> {
    read_core(state.0.clone(), move |library| library.list_asset_tags(&asset_id)).await
}

#[tauri::command]
async fn tags_list(state: State<'_, CoreState>) -> Result<Vec<String>, ApiError> {
    read_core(state.0.clone(), |library| library.list_tag_names()).await
}

#[tauri::command]
async fn thumbnail_regenerate(state: State<'_, CoreState>, asset_id: String) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), move |library| library.regenerate_thumbnail(&asset_id)).await
}

#[tauri::command]
async fn thumbnails_regenerate_all(state: State<'_, CoreState>) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), |library| library.regenerate_all_thumbnails()).await
}

#[tauri::command]
fn tag_add(
    state: State<'_, CoreState>,
    asset_id: String,
    name: String,
    source: Option<String>,
) -> Result<TagMutation, ApiError> {
    state
        .0
        .add_asset_tag(&asset_id, &name, source.as_deref().unwrap_or("user"), None)
        .map_err(Into::into)
}

#[tauri::command]
fn tag_add_batch(
    state: State<'_, CoreState>,
    asset_ids: Vec<String>,
    name: String,
    source: Option<String>,
) -> Result<Vec<TagMutation>, ApiError> {
    state
        .0
        .add_asset_tag_batch(&asset_ids, &name, source.as_deref().unwrap_or("user"), None)
        .map_err(Into::into)
}

#[tauri::command]
fn tag_remove(
    state: State<'_, CoreState>,
    asset_id: String,
    name: String,
) -> Result<TagMutation, ApiError> {
    state
        .0
        .remove_asset_tag(&asset_id, &name)
        .map_err(Into::into)
}

#[tauri::command]
fn tag_remove_batch(
    state: State<'_, CoreState>,
    asset_ids: Vec<String>,
    name: String,
) -> Result<Vec<TagMutation>, ApiError> {
    state
        .0
        .remove_asset_tag_batch(&asset_ids, &name)
        .map_err(Into::into)
}

#[tauri::command]
fn favorite_set(
    state: State<'_, CoreState>,
    asset_id: String,
    favorite: bool,
) -> Result<bool, ApiError> {
    state
        .0
        .set_favorite(&asset_id, favorite)
        .map_err(Into::into)
}

#[tauri::command]
fn favorite_set_batch(
    state: State<'_, CoreState>,
    asset_ids: Vec<String>,
    favorite: bool,
) -> Result<usize, ApiError> {
    state
        .0
        .set_favorites_batch(&asset_ids, favorite)
        .map_err(Into::into)
}

#[tauri::command]
fn favorites_list(
    state: State<'_, CoreState>,
    query: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Asset>, ApiError> {
    state
        .0
        .favorite_assets(query.as_deref(), limit.unwrap_or(500))
        .map_err(Into::into)
}

#[tauri::command]
async fn relations_list(
    state: State<'_, CoreState>,
    asset_id: Option<String>,
    relation_type: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<AssetRelation>, ApiError> {
    read_core(state.0.clone(), move |library| {
        library.list_relations(asset_id.as_deref(), relation_type.as_deref(), limit.unwrap_or(500))
    }).await
}

#[tauri::command]
fn relations_refresh(state: State<'_, CoreState>) -> Result<RelationRefreshReport, ApiError> {
    state.0.rebuild_relations().map_err(Into::into)
}

#[tauri::command]
fn relation_confirm(state: State<'_, CoreState>, relation_id: String) -> Result<bool, ApiError> {
    state.0.confirm_relation(&relation_id).map_err(Into::into)
}

#[tauri::command]
async fn filters_list(state: State<'_, CoreState>) -> Result<Vec<SavedFilter>, ApiError> {
    read_core(state.0.clone(), |library| library.list_saved_filters()).await
}

#[tauri::command]
fn filter_save(
    state: State<'_, CoreState>,
    filter_id: Option<String>,
    name: String,
    expression: FilterExpr,
) -> Result<SavedFilter, ApiError> {
    state
        .0
        .save_filter(filter_id.as_deref(), &name, expression)
        .map_err(Into::into)
}

#[tauri::command]
fn filter_remove(state: State<'_, CoreState>, filter_id: String) -> Result<bool, ApiError> {
    state.0.remove_saved_filter(&filter_id).map_err(Into::into)
}

#[tauri::command]
fn assets_filter(
    state: State<'_, CoreState>,
    filter_id: String,
    query: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<Asset>, ApiError> {
    state
        .0
        .apply_saved_filter(&filter_id, query.as_deref(), limit.unwrap_or(500))
        .map_err(Into::into)
}

#[tauri::command]
async fn jobs_list(state: State<'_, CoreState>) -> Result<Vec<serde_json::Value>, ApiError> {
    read_core(state.0.clone(), |library| library.list_jobs()).await
}

#[tauri::command]
async fn jobs_summary(state: State<'_, CoreState>) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), |library| library.job_summary()).await
}

#[tauri::command]
async fn storage_info(state: State<'_, CoreState>) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), |library| library.storage_info()).await
}

#[tauri::command]
async fn storage_compact(state: State<'_, CoreState>) -> Result<serde_json::Value, ApiError> {
    read_core(state.0.clone(), |library| library.compact_storage()).await
}

#[tauri::command]
fn jobs_cancel(state: State<'_, CoreState>, job_id: String) -> Result<bool, ApiError> {
    state.0.cancel_job(&job_id).map_err(Into::into)
}

#[tauri::command]
fn jobs_retry(state: State<'_, CoreState>, job_id: String) -> Result<serde_json::Value, ApiError> {
    state.0.retry_job(&job_id).map_err(Into::into)
}

fn main() {
    let result = tauri::Builder::default()
        .setup(|app| {
            let started = std::time::Instant::now();
            let status = std::sync::Arc::new(std::sync::Mutex::new(StartupStatus {
                phase: "正在打开本地资产库…".to_owned(), ready: false, error: None,
                detail: "准备数据库初始化线程".to_owned(), step: 0, completed: None, total: None,
                elapsed_ms: 0, phase_elapsed_ms: 0, idle_ms: 0, phase_started_ms: 0, updated_ms: 0,
            }));
            app.manage(StartupState { status: status.clone(), started });
            let app_handle = app.handle().clone();
            std::thread::Builder::new()
                .name("mmdbridge-background-startup".to_owned())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let database = Library::portable_database_path()?;
                        let mut log = std::fs::OpenOptions::new().create(true).write(true).truncate(true)
                            .open(database.with_file_name("startup-progress.log")).ok();
                        Library::open_with_progress(database, &mut |progress| {
                            let elapsed = started.elapsed().as_millis() as u64;
                            let mut phase_changed = false;
                            if let Ok(mut snapshot) = status.lock() {
                                phase_changed = snapshot.phase != progress.phase;
                                if phase_changed { snapshot.phase_started_ms = elapsed; }
                                snapshot.phase = progress.phase.clone();
                                snapshot.detail = progress.detail.clone();
                                snapshot.step = progress.step;
                                snapshot.completed = progress.completed;
                                snapshot.total = progress.total;
                                snapshot.updated_ms = elapsed;
                            }
                            if phase_changed || progress.completed == progress.total
                                || progress.completed.is_some_and(|count| count % 1000 == 0) {
                                if let Some(log) = log.as_mut() {
                                    use std::io::Write;
                                    let _ = writeln!(log, "{elapsed}ms stage {}/6: {} {:?}/{:?}; {}",
                                        progress.step, progress.phase, progress.completed, progress.total, progress.detail);
                                }
                            }
                        })
                    })).unwrap_or_else(|panic| {
                        let message = panic.downcast_ref::<&str>().copied()
                            .or_else(|| panic.downcast_ref::<String>().map(String::as_str)).unwrap_or("未知异常");
                        Err(CoreError::Io(std::io::Error::other(format!("启动线程异常：{message}"))))
                    });
                    match result {
                        Ok(library) => {
                            app_handle.manage(CoreState(library, std::sync::atomic::AtomicBool::new(false)));
                            if let Ok(mut status) = status.lock() {
                                status.ready = true;
                                status.phase = "资产库已准备好".to_owned();
                            }
                        }
                        Err(error) => {
                            let message = format!("资产库初始化失败：{error}");
                            if let Ok(database) = Library::portable_database_path() {
                                let _ = std::fs::write(database.with_file_name("startup-error.txt"), &message);
                            }
                            if let Ok(mut status) = status.lock() { status.error = Some(message); }
                        }
                    }
                })?;
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            startup_status,
            library_background_start,
            tags_list,
            thumbnail_regenerate,
            thumbnails_regenerate_all,
            roots_list,
            root_add,
            root_update,
            root_remove,
            scan_enqueue,
            scan_full_check,
            scan_states,
            scan_cancel,
            scan_pause,
            scan_continue,
            scan_move,
            model_preview,
            motion_preview_frame,
            model_preview_file,
            scene_preview,
            scene_preview_texture,
            model_preview_texture_file,
            motion_preview_model_get,
            motion_preview_model_set,
            thumbnail_concurrency_get,
            auto_tag_settings_get,
            auto_tag_settings_set,
            thumbnail_concurrency_set,
            asset_operation_plan,
            asset_operation_execute,
            operation_journal_list,
            operation_journal_resolve,
            assets_list,
            asset_counts,
            asset_directories,
            asset_directory_page,
            assets_page,
            asset_reveal,
            asset_open_directory,
            asset_inspect,
            cards_pending,
            card_create,
            card_thumbnail,
            card_verify,
            cards_queue_root,
            thumbnail_enqueue,
            thumbnail_batch,
            asset_tags,
            tag_add,
            tag_add_batch,
            tag_remove,
            tag_remove_batch,
            favorite_set,
            favorite_set_batch,
            favorites_list,
            relations_list,
            relations_refresh,
            relation_confirm,
            filters_list,
            filter_save,
            filter_remove,
            assets_filter,
            jobs_list,
            jobs_summary,
            storage_info,
            storage_compact,
            jobs_cancel,
            jobs_retry
        ])
        .run(tauri::generate_context!());
    if let Err(error) = result {
        let message = format!("MMDbridgeLib 启动失败：{error}");
        if let Ok(database) = Library::portable_database_path() {
            let _ = std::fs::write(database.with_file_name("startup-error.txt"), &message);
        }
        eprintln!("{message}");
        std::process::exit(1);
    }
}
