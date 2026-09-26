use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    thread,
};

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{CoreError, CoreResult, Library};

const MAX_BATCH_ASSETS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationAsset {
    pub id: String,
    pub name: String,
    pub primary_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationPlan {
    pub operation: String,
    pub asset_ids: Vec<String>,
    pub source_paths: Vec<String>,
    pub destination_paths: Vec<String>,
    pub destination_parent: Option<String>,
    pub new_name: Option<String>,
    pub affected_assets: Vec<AssetOperationAsset>,
    pub dependency_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub can_execute: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationJournalEntry {
    pub id: String,
    pub operation: String,
    pub status: String,
    pub source_paths: Vec<String>,
    pub destination_paths: Vec<String>,
    pub affected_asset_count: usize,
    pub created_at: String,
    pub updated_at: String,
    pub result: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    Move,
    Rename,
    Recycle,
}

impl OperationKind {
    fn parse(value: &str) -> CoreResult<Self> {
        match value {
            "move" => Ok(Self::Move),
            "rename" => Ok(Self::Rename),
            "recycle" => Ok(Self::Recycle),
            _ => Err(CoreError::AssetOperation(format!(
                "unsupported operation: {value}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Rename => "rename",
            Self::Recycle => "recycle",
        }
    }
}

#[derive(Debug, Clone)]
struct StoredAsset {
    id: String,
    name: String,
    primary_source: String,
    asset_directory: String,
    root_id: String,
    root_path: String,
    asset_type: String,
}

#[derive(Debug, Clone)]
struct Package {
    source: PathBuf,
    target: Option<PathBuf>,
    assets: Vec<StoredAsset>,
}

struct PreparedPlan {
    view: AssetOperationPlan,
    kind: OperationKind,
    packages: Vec<Package>,
    affected_assets: Vec<StoredAsset>,
}

#[derive(Debug, Clone)]
enum FileAction {
    Move { source: PathBuf, target: PathBuf },
    Recycle { source: PathBuf },
}

pub(crate) fn plan(
    library: &Library,
    operation: &str,
    asset_ids: &[String],
    destination_parent: Option<&str>,
    new_name: Option<&str>,
) -> CoreResult<AssetOperationPlan> {
    Ok(prepare(library, operation, asset_ids, destination_parent, new_name)?.view)
}

fn prepare(
    library: &Library,
    operation: &str,
    asset_ids: &[String],
    destination_parent: Option<&str>,
    new_name: Option<&str>,
) -> CoreResult<PreparedPlan> {
    let kind = OperationKind::parse(operation)?;
    let mut requested_ids = asset_ids
        .iter()
        .map(|asset_id| asset_id.trim().to_owned())
        .filter(|asset_id| !asset_id.is_empty())
        .collect::<Vec<_>>();
    requested_ids.sort();
    requested_ids.dedup();
    if requested_ids.is_empty() || requested_ids.len() > MAX_BATCH_ASSETS {
        return Err(CoreError::AssetOperation(format!(
            "operation requires 1 to {MAX_BATCH_ASSETS} unique assets"
        )));
    }
    if kind == OperationKind::Rename && requested_ids.len() != 1 {
        return Err(CoreError::AssetOperation(
            "rename accepts one asset package at a time".to_owned(),
        ));
    }
    let requested_name = if kind == OperationKind::Rename {
        Some(validate_name(new_name.unwrap_or_default())?)
    } else {
        None
    };
    let destination_parent = if kind == OperationKind::Move {
        Some(canonical_directory(destination_parent.ok_or_else(
            || CoreError::AssetOperation("move requires an existing destination folder".to_owned()),
        )?)?)
    } else {
        None
    };

    let selected_assets = requested_ids
        .iter()
        .map(|asset_id| load_asset(library, asset_id))
        .collect::<CoreResult<Vec<_>>>()?;
    let mut warnings = Vec::<String>::new();
    let mut sources = Vec::<PathBuf>::new();
    for asset in &selected_assets {
        let root = canonical_directory(&asset.root_path)?;
        let package = canonical_directory(&asset.asset_directory)?;
        let primary = canonical_file(&asset.primary_source)?;
        if !path_is_within(&root, &package) || same_path(&root, &package) {
            return Err(CoreError::AssetOperation(format!(
                "资产包必须位于资产根目录的子文件夹，无法对根目录中的散落资产单独操作：{}",
                package.display()
            )));
        }
        if !path_is_within(&package, &primary) {
            return Err(CoreError::AssetOperation(format!(
                "主文件不在资产包目录内：{}",
                primary.display()
            )));
        }
        if sources.iter().all(|source| !same_path(source, &package)) {
            sources.push(package);
        }
    }
    sources.sort_by_key(|path| path.components().count());
    let mut collapsed_sources = Vec::<PathBuf>::new();
    for source in sources {
        if collapsed_sources
            .iter()
            .any(|parent| path_is_within(parent, &source))
        {
            continue;
        }
        collapsed_sources.push(source);
    }

    let all_roots = load_roots(library)?;
    let mut packages = Vec::new();
    let mut affected_assets = Vec::<StoredAsset>::new();
    let mut dependency_paths = HashSet::<String>::new();
    for source in collapsed_sources {
        for (root_id, root_path) in &all_roots {
            if selected_assets
                .iter()
                .any(|asset| &asset.root_id == root_id)
            {
                continue;
            }
            if let Ok(root_path) = fs::canonicalize(root_path)
                && path_is_within(&source, &root_path)
            {
                warnings.push(format!(
                    "资产包包含另一个已登记的资产根目录，不能移动或删除：{}",
                    root_path.display()
                ));
            }
        }
        for symlink in package_symlinks(&source)? {
            warnings.push(format!(
                "资产包包含符号链接或重解析点，操作已禁用：{}",
                symlink.display()
            ));
        }
        let package_assets = load_assets_under_package(library, &source)?;
        if package_assets.is_empty() {
            warnings.push(format!("找不到资产包中的索引记录：{}", source.display()));
        }
        if kind != OperationKind::Recycle
            && package_assets
                .iter()
                .map(|asset| asset.asset_type.as_str())
                .collect::<HashSet<_>>()
                .len()
                > 1
        {
            warnings.push(format!(
                "同一资产包在 Library 中登记为多种资产类型，无法安全更新根目录索引：{}",
                source.display()
            ));
        }
        for asset in &package_assets {
            affected_assets.push(asset.clone());
            let dependency_rows = load_asset_dependencies(library, &asset.id)?;
            for dependency in dependency_rows {
                match fs::canonicalize(&dependency) {
                    Ok(canonical) if path_is_within(&source, &canonical) => {
                        dependency_paths.insert(canonical.to_string_lossy().into_owned());
                    }
                    Ok(canonical) => warnings.push(format!(
                        "检测到资产包外依赖，需先收拢到包内再操作：{}",
                        canonical.display()
                    )),
                    Err(_) => warnings.push(format!(
                        "索引中的依赖文件已不存在，请重新扫描后再操作：{}",
                        dependency.display()
                    )),
                }
            }
            if let Some(card_path) = load_card_path(library, &asset.id)?
                && !path_is_within(&source, Path::new(&card_path))
                && Path::new(&card_path).exists()
            {
                warnings.push(format!("资源卡位于资产包目录之外：{card_path}"));
            }
            if has_active_job(library, &asset.id)? {
                warnings.push(format!(
                    "资产仍有后台任务运行，请等待任务完成后再操作：{}",
                    asset.name
                ));
            }
            if has_unresolved_operation(library, &asset.id)? {
                warnings.push(format!(
                    "该资产有待人工核对的文件操作记录，请先恢复文件并重新扫描：{}",
                    asset.name
                ));
            }
        }
        packages.push(Package {
            source,
            target: None,
            assets: package_assets,
        });
    }
    affected_assets.sort_by(|left, right| left.id.cmp(&right.id));
    affected_assets.dedup_by(|left, right| left.id == right.id);

    if kind == OperationKind::Recycle && !cfg!(windows) {
        warnings.push("此平台没有启用回收站接口；为避免永久删除，不能执行删除".to_owned());
    }

    let mut destination_paths = Vec::<PathBuf>::new();
    match kind {
        OperationKind::Move => {
            let destination_parent = destination_parent
                .as_ref()
                .expect("move destination parent was validated");
            for package in &mut packages {
                let Some(name) = package.source.file_name() else {
                    warnings.push(format!(
                        "无法读取资产包目录名：{}",
                        package.source.display()
                    ));
                    continue;
                };
                let target = destination_parent.join(name);
                if same_path(&package.source, &target) {
                    warnings.push(format!(
                        "资产包已经位于目标位置：{}",
                        package.source.display()
                    ));
                } else if path_is_within(&package.source, destination_parent) {
                    warnings.push(format!(
                        "不能把资产包移动到它自己的子目录：{}",
                        package.source.display()
                    ));
                } else if target.exists() {
                    warnings.push(format!("目标位置已存在：{}", target.display()));
                }
                for asset in &package.assets {
                    if find_root_for_path(library, destination_parent, &asset.asset_type)?.is_none()
                    {
                        warnings.push(format!(
                            "目标目录不属于已登记的 {} 根目录：{}",
                            asset.asset_type,
                            destination_parent.display()
                        ));
                    }
                }
                package.target = Some(target.clone());
                destination_paths.push(target);
            }
        }
        OperationKind::Rename => {
            let new_name = requested_name
                .as_deref()
                .expect("rename name was validated");
            for package in &mut packages {
                let Some(parent) = package.source.parent() else {
                    warnings.push(format!(
                        "无法读取资产包父目录：{}",
                        package.source.display()
                    ));
                    continue;
                };
                let target = parent.join(new_name);
                let source_text = package.source.to_string_lossy();
                let target_text = target.to_string_lossy();
                if source_text == target_text {
                    warnings.push("新名称与当前名称相同".to_owned());
                } else if target.exists() && !same_path(&package.source, &target) {
                    warnings.push(format!("目标位置已存在：{}", target.display()));
                }
                package.target = Some(target.clone());
                destination_paths.push(target);
            }
        }
        OperationKind::Recycle => {}
    }
    let mut targets_seen = HashSet::<String>::new();
    for target in &destination_paths {
        if !targets_seen.insert(path_key(target)) {
            warnings.push(format!("多个资产包会映射到同一目标：{}", target.display()));
        }
    }
    warnings.sort();
    warnings.dedup();
    let mut dependency_paths = dependency_paths.into_iter().collect::<Vec<_>>();
    dependency_paths.sort();

    let view = AssetOperationPlan {
        operation: kind.as_str().to_owned(),
        asset_ids: requested_ids,
        source_paths: packages
            .iter()
            .map(|package| package.source.to_string_lossy().into_owned())
            .collect(),
        destination_paths: destination_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        destination_parent: destination_parent
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        new_name: requested_name,
        affected_assets: affected_assets
            .iter()
            .map(|asset| AssetOperationAsset {
                id: asset.id.clone(),
                name: asset.name.clone(),
                primary_source: asset.primary_source.clone(),
            })
            .collect(),
        dependency_paths,
        warnings: warnings.clone(),
        can_execute: warnings.is_empty(),
    };
    Ok(PreparedPlan {
        view,
        kind,
        packages,
        affected_assets,
    })
}

pub(crate) fn execute(
    library: &Library,
    confirmed_plan: &AssetOperationPlan,
) -> CoreResult<AssetOperationJournalEntry> {
    let _operation_guard = library.asset_operation_guard()?;
    let prepared = prepare(
        library,
        &confirmed_plan.operation,
        &confirmed_plan.asset_ids,
        confirmed_plan.destination_parent.as_deref(),
        confirmed_plan.new_name.as_deref(),
    )?;
    if !prepared.view.can_execute {
        return Err(CoreError::AssetOperation(format!(
            "操作计划不可执行：{}",
            prepared.view.warnings.join("；")
        )));
    }
    if prepared.view.source_paths != confirmed_plan.source_paths
        || prepared.view.destination_paths != confirmed_plan.destination_paths
        || prepared.view.affected_assets != confirmed_plan.affected_assets
        || prepared.view.dependency_paths != confirmed_plan.dependency_paths
        || prepared.view.warnings != confirmed_plan.warnings
    {
        return Err(CoreError::AssetOperation(
            "资产包在确认后发生变化，请重新生成操作计划".to_owned(),
        ));
    }

    let operation_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let asset_ids = prepared
        .affected_assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    insert_journal(
        library,
        &operation_id,
        prepared.kind,
        "Started",
        &asset_ids,
        &prepared.view.source_paths,
        &prepared.view.destination_paths,
        &now,
        json!({"message":"Operation started"}),
    )?;

    if let Some(asset) = prepared
        .affected_assets
        .iter()
        .find(|asset| has_active_job(library, &asset.id).unwrap_or(true))
    {
        let message = format!("资产新增了后台任务，文件操作未执行：{}", asset.name);
        finish_journal(library, &operation_id, "Failed", json!({"message":message}))?;
        return Err(CoreError::AssetOperation(message));
    }

    let actions = make_actions(&prepared);
    let completed = match execute_actions(&actions) {
        Ok(completed) => completed,
        Err((message, completed)) => {
            let status = if prepared.kind == OperationKind::Recycle || !completed.is_empty() {
                "RecoveryNeeded"
            } else {
                "Failed"
            };
            finish_journal(library, &operation_id, status, json!({"message":message}))?;
            return Err(CoreError::AssetOperation(message));
        }
    };

    let index_warnings = match apply_index_change(library, &prepared) {
        Ok(warnings) => warnings,
        Err(error) => {
            if prepared.kind == OperationKind::Recycle {
                let message = format!(
                    "文件已发送到回收站，但索引更新失败；请从操作日志查看源路径并恢复：{error}"
                );
                finish_journal(
                    library,
                    &operation_id,
                    "RecoveryNeeded",
                    json!({"message":message}),
                )?;
                return Err(CoreError::AssetOperation(message));
            }
            let rollback = rollback_moves(&completed);
            let (status, message) = match rollback {
                Ok(()) => ("Failed", format!("索引更新失败，已回滚文件移动：{error}")),
                Err(rollback_error) => (
                    "RecoveryNeeded",
                    format!("索引更新与文件回滚均失败：{error}；{rollback_error}"),
                ),
            };
            finish_journal(library, &operation_id, status, json!({"message":message}))?;
            return Err(CoreError::AssetOperation(message));
        }
    };

    let mut message = match prepared.kind {
        OperationKind::Move => "资产包已移动".to_owned(),
        OperationKind::Rename => "资产包已重命名".to_owned(),
        OperationKind::Recycle => "资产包已发送到 Windows 回收站".to_owned(),
    };
    if !index_warnings.is_empty() {
        message.push_str("；索引更新警告：");
        message.push_str(&index_warnings.join("；"));
    }
    let result = json!({"message":message});
    finish_journal(library, &operation_id, "Completed", result.clone())?;
    Ok(AssetOperationJournalEntry {
        id: operation_id,
        operation: prepared.kind.as_str().to_owned(),
        status: "Completed".to_owned(),
        source_paths: prepared.view.source_paths,
        destination_paths: prepared.view.destination_paths,
        affected_asset_count: asset_ids.len(),
        created_at: now.clone(),
        updated_at: Utc::now().to_rfc3339(),
        result,
    })
}

pub(crate) fn mark_interrupted(library: &Library) -> CoreResult<()> {
    let connection = library.connection()?;
    connection.execute(
        "UPDATE operation_journal SET status='RecoveryNeeded',updated_at=?1,
         result_json=json_object('message','Application stopped before this operation was confirmed complete; inspect the source and destination, restore files if needed, then rescan before resolving this entry')
         WHERE status='Started'",
        [Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub(crate) fn resolve_journal(library: &Library, id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE operation_journal SET status='Resolved',updated_at=?1,
         result_json=json_object('message','User confirmed manual recovery and rescan')
         WHERE id=?2 AND status='RecoveryNeeded'",
        params![Utc::now().to_rfc3339(), id],
    )?;
    Ok(changed > 0)
}

fn make_actions(prepared: &PreparedPlan) -> Vec<FileAction> {
    prepared
        .packages
        .iter()
        .map(|package| match prepared.kind {
            OperationKind::Move | OperationKind::Rename => FileAction::Move {
                source: package.source.clone(),
                target: package
                    .target
                    .as_ref()
                    .expect("move and rename plans have targets")
                    .clone(),
            },
            OperationKind::Recycle => FileAction::Recycle {
                source: package.source.clone(),
            },
        })
        .collect()
}

fn execute_actions(actions: &[FileAction]) -> Result<Vec<FileAction>, (String, Vec<FileAction>)> {
    let mut completed = Vec::new();
    for action in actions {
        if let Err(error) = perform_file_action(action) {
            if let FileAction::Move { source, target } = action
                && !source.exists()
                && target.exists()
            {
                completed.push(action.clone());
            }
            return Err((error, completed));
        }
        completed.push(action.clone());
    }
    Ok(completed)
}

fn rollback_moves(completed: &[FileAction]) -> Result<(), String> {
    for action in completed.iter().rev() {
        let FileAction::Move { source, target } = action else {
            return Err("delete operations cannot be rolled back by the application".to_owned());
        };
        perform_file_action(&FileAction::Move {
            source: target.clone(),
            target: source.clone(),
        })?;
    }
    Ok(())
}

fn apply_index_change(library: &Library, prepared: &PreparedPlan) -> CoreResult<Vec<String>> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    if prepared.kind == OperationKind::Recycle {
        for asset in &prepared.affected_assets {
            transaction.execute("DELETE FROM assets WHERE id=?1", [&asset.id])?;
        }
        transaction.commit()?;
        drop(connection);
        return Ok(refresh_derived_indices(library));
    }

    let mut moved_path_map = HashMap::<String, String>::new();
    for package in &prepared.packages {
        let target = package
            .target
            .as_ref()
            .expect("move and rename plans have targets");
        for asset in &package.assets {
            let root_id = if prepared.kind == OperationKind::Move {
                find_root_for_path_in_transaction(
                    &transaction,
                    target.parent().unwrap_or(target),
                    &asset.asset_type,
                )?
                .ok_or_else(|| {
                    CoreError::AssetOperation(format!(
                        "移动目标已不在已登记的 {} 根目录中：{}",
                        asset.asset_type,
                        target.display()
                    ))
                })?
            } else {
                asset.root_id.clone()
            };
            let primary = remap_path(&asset.primary_source, &package.source, target)?;
            let asset_directory = remap_path(&asset.asset_directory, &package.source, target)?;
            let old_statuses: String = transaction.query_row(
                "SELECT statuses_json FROM assets WHERE id=?1",
                [&asset.id],
                |row| row.get(0),
            )?;
            let mut statuses =
                serde_json::from_str::<Vec<String>>(&old_statuses).unwrap_or_default();
            statuses.retain(|status| status != "MissingSource");
            let statuses_json = serde_json::to_string(&statuses)?;
            moved_path_map.insert(asset.primary_source.clone(), primary.clone());
            moved_path_map.insert(asset.asset_directory.clone(), asset_directory.clone());
            transaction.execute(
                "UPDATE assets SET root_id=?2,primary_source=?3,asset_directory=?4,statuses_json=?5,updated_at=?6,last_seen_at=?6 WHERE id=?1",
                params![asset.id, root_id, primary, asset_directory, statuses_json, Utc::now().to_rfc3339()],
            )?;
            let file_paths = {
                let mut statement =
                    transaction.prepare("SELECT path FROM asset_files WHERE asset_id=?1")?;
                statement
                    .query_map([&asset.id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for old_path in file_paths {
                let new_path = remap_path(&old_path, &package.source, target)?;
                moved_path_map.insert(old_path.clone(), new_path.clone());
                transaction.execute(
                    "UPDATE asset_files SET path=?3 WHERE asset_id=?1 AND path=?2",
                    params![asset.id, old_path, new_path],
                )?;
            }
            let card_path: Option<String> = transaction
                .query_row(
                    "SELECT card_path FROM cards WHERE asset_id=?1",
                    [&asset.id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(card_path) = card_path {
                let new_card_path = remap_path(&card_path, &package.source, target)?;
                moved_path_map.insert(card_path, new_card_path.clone());
                transaction.execute(
                    "UPDATE cards SET card_path=?2,status=CASE WHEN status='CardMissing' THEN status ELSE 'CardStale' END,last_checked_at=?3 WHERE asset_id=?1",
                    params![asset.id, new_card_path, Utc::now().to_rfc3339()],
                )?;
            }
        }
    }
    rewrite_relation_paths(&transaction, &moved_path_map)?;
    transaction.commit()?;
    drop(connection);
    Ok(refresh_derived_indices(library))
}

fn refresh_derived_indices(library: &Library) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Err(error) = crate::relations::rebuild(library) {
        warnings.push(format!("关系建议刷新失败：{error}"));
    }
    if let Err(error) = crate::duplicates::rebuild(library) {
        warnings.push(format!("重复项索引刷新失败：{error}"));
    }
    warnings
}

fn rewrite_relation_paths(
    transaction: &rusqlite::Transaction<'_>,
    moved_paths: &HashMap<String, String>,
) -> CoreResult<()> {
    let rows = {
        let mut statement = transaction.prepare("SELECT id,reason_json FROM relations")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, reason_json) in rows {
        let Ok(mut reason) = serde_json::from_str::<Value>(&reason_json) else {
            continue;
        };
        let old = reason.clone();
        rewrite_json_paths(&mut reason, moved_paths);
        if reason != old {
            transaction.execute(
                "UPDATE relations SET reason_json=?2 WHERE id=?1",
                params![id, serde_json::to_string(&reason)?],
            )?;
        }
    }
    Ok(())
}

fn rewrite_json_paths(value: &mut Value, moved_paths: &HashMap<String, String>) {
    match value {
        Value::String(text) => {
            if let Some((_, new_path)) = moved_paths
                .iter()
                .find(|(old_path, _)| path_key(Path::new(old_path)) == path_key(Path::new(text)))
            {
                *text = new_path.clone();
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_json_paths(item, moved_paths);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                rewrite_json_paths(item, moved_paths);
            }
        }
        _ => {}
    }
}

fn remap_path(path: &str, source: &Path, target: &Path) -> CoreResult<String> {
    let path = Path::new(path);
    let relative = path.strip_prefix(source).map_err(|_| {
        CoreError::AssetOperation(format!(
            "资产依赖不在包目录内，已拒绝更新路径：{}",
            path.display()
        ))
    })?;
    Ok(target.join(relative).to_string_lossy().into_owned())
}

fn insert_journal(
    library: &Library,
    id: &str,
    operation: OperationKind,
    status: &str,
    asset_ids: &[String],
    sources: &[String],
    destinations: &[String],
    timestamp: &str,
    result: Value,
) -> CoreResult<()> {
    let connection = library.connection()?;
    connection.execute(
        "INSERT INTO operation_journal(id,operation,status,asset_ids_json,sources_json,destinations_json,created_at,updated_at,result_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?8)",
        params![
            id,
            operation.as_str(),
            status,
            serde_json::to_string(asset_ids)?,
            serde_json::to_string(sources)?,
            serde_json::to_string(destinations)?,
            timestamp,
            serde_json::to_string(&result)?
        ],
    )?;
    Ok(())
}

fn finish_journal(library: &Library, id: &str, status: &str, result: Value) -> CoreResult<()> {
    let connection = library.connection()?;
    connection.execute(
        "UPDATE operation_journal SET status=?2,updated_at=?3,result_json=?4 WHERE id=?1",
        params![
            id,
            status,
            Utc::now().to_rfc3339(),
            serde_json::to_string(&result)?
        ],
    )?;
    Ok(())
}

pub(crate) fn list_journal(
    library: &Library,
    limit: usize,
) -> CoreResult<Vec<AssetOperationJournalEntry>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT id,operation,status,sources_json,destinations_json,asset_ids_json,created_at,updated_at,result_json
         FROM operation_journal ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = statement.query_map([i64::try_from(limit.clamp(1, 500)).unwrap_or(100)], |row| {
        let sources: String = row.get(3)?;
        let destinations: String = row.get(4)?;
        let assets: String = row.get(5)?;
        let result: String = row.get(8)?;
        Ok(AssetOperationJournalEntry {
            id: row.get(0)?,
            operation: row.get(1)?,
            status: row.get(2)?,
            source_paths: serde_json::from_str(&sources).unwrap_or_default(),
            destination_paths: serde_json::from_str(&destinations).unwrap_or_default(),
            affected_asset_count: serde_json::from_str::<Vec<String>>(&assets)
                .map(|values| values.len())
                .unwrap_or_default(),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            result: serde_json::from_str(&result).unwrap_or(Value::Null),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn load_asset(library: &Library, asset_id: &str) -> CoreResult<StoredAsset> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT a.id,a.name,a.primary_source,a.asset_directory,a.root_id,r.path,r.asset_type
             FROM assets a JOIN roots r ON r.id=a.root_id WHERE a.id=?1",
            [asset_id],
            |row| {
                Ok(StoredAsset {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    primary_source: row.get(2)?,
                    asset_directory: row.get(3)?,
                    root_id: row.get(4)?,
                    root_path: row.get(5)?,
                    asset_type: row.get(6)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| CoreError::AssetNotFound(asset_id.to_owned()))
}

fn load_roots(library: &Library) -> CoreResult<Vec<(String, String)>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare("SELECT id,path FROM roots")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn load_assets_under_package(library: &Library, package: &Path) -> CoreResult<Vec<StoredAsset>> {
    let package_text = package.to_string_lossy().into_owned();
    let prefix = format!(
        "{}{}",
        package_text.trim_end_matches(['\\', '/']),
        std::path::MAIN_SEPARATOR
    );
    let pattern = format!("{}%", escape_like(&prefix));
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.name,a.primary_source,a.asset_directory,a.root_id,r.path,r.asset_type
         FROM assets a JOIN roots r ON r.id=a.root_id
         WHERE a.asset_directory=?1 COLLATE NOCASE OR a.asset_directory LIKE ?2 ESCAPE '!' COLLATE NOCASE
         ORDER BY a.id",
    )?;
    let rows = statement.query_map(params![package_text, pattern], |row| {
        Ok(StoredAsset {
            id: row.get(0)?,
            name: row.get(1)?,
            primary_source: row.get(2)?,
            asset_directory: row.get(3)?,
            root_id: row.get(4)?,
            root_path: row.get(5)?,
            asset_type: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn load_asset_dependencies(library: &Library, asset_id: &str) -> CoreResult<Vec<PathBuf>> {
    let connection = library.connection()?;
    let mut statement =
        connection.prepare("SELECT path FROM asset_files WHERE asset_id=?1 AND role<>'primary'")?;
    let rows = statement.query_map([asset_id], |row| row.get::<_, String>(0))?;
    rows.map(|row| row.map(PathBuf::from))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn load_card_path(library: &Library, asset_id: &str) -> CoreResult<Option<String>> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT card_path FROM cards WHERE asset_id=?1",
            [asset_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn has_active_job(library: &Library, asset_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE asset_id=?1 AND status IN ('Pending','Parsing','Rendering','Encoding'))",
            [asset_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn has_unresolved_operation(library: &Library, asset_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM operation_journal j, json_each(j.asset_ids_json) ids
               WHERE j.status='RecoveryNeeded' AND ids.value=?1
             )",
            [asset_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn is_asset_operation_active(library: &Library, asset_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM operation_journal j, json_each(j.asset_ids_json) ids
               WHERE j.status IN ('Started','RecoveryNeeded') AND ids.value=?1
             )",
            [asset_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn find_root_for_path(
    library: &Library,
    path: &Path,
    asset_type: &str,
) -> CoreResult<Option<String>> {
    let connection = library.connection()?;
    let mut statement = connection.prepare("SELECT id,path FROM roots WHERE asset_type=?1")?;
    let rows = statement.query_map([asset_type], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matching = Vec::<(usize, String)>::new();
    for row in rows {
        let (id, path_text) = row?;
        if let Ok(root) = fs::canonicalize(path_text)
            && path_is_within(&root, path)
        {
            matching.push((root.components().count(), id));
        }
    }
    matching.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    Ok(matching.into_iter().next().map(|(_, id)| id))
}

fn find_root_for_path_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    path: &Path,
    asset_type: &str,
) -> CoreResult<Option<String>> {
    let mut statement = transaction.prepare("SELECT id,path FROM roots WHERE asset_type=?1")?;
    let rows = statement.query_map([asset_type], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut matching = Vec::<(usize, String)>::new();
    for row in rows {
        let (id, path_text) = row?;
        if let Ok(root) = fs::canonicalize(path_text)
            && path_is_within(&root, path)
        {
            matching.push((root.components().count(), id));
        }
    }
    matching.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    Ok(matching.into_iter().next().map(|(_, id)| id))
}

fn canonical_directory(path: &str) -> CoreResult<PathBuf> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| CoreError::AssetOperation(format!("无法访问目录 {path}：{error}")))?;
    if !canonical.is_dir() {
        return Err(CoreError::AssetOperation(format!(
            "路径不是目录：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn canonical_file(path: &str) -> CoreResult<PathBuf> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| CoreError::AssetOperation(format!("无法访问文件 {path}：{error}")))?;
    if !canonical.is_file() {
        return Err(CoreError::AssetOperation(format!(
            "路径不是文件：{}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn validate_name(value: &str) -> CoreResult<String> {
    let name = value.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 240
        || name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
        || name.ends_with('.')
        || name.ends_with(' ')
    {
        return Err(CoreError::AssetOperation(
            "新名称不能为空，且不得包含 Windows 路径保留字符、控制字符或尾随点/空格".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

fn package_symlinks(package: &Path) -> CoreResult<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(package).follow_links(false) {
        let entry = entry
            .map_err(|error| CoreError::AssetOperation(format!("无法完整检查资产包：{error}")))?;
        if entry.file_type().is_symlink() {
            found.push(entry.path().to_path_buf());
        }
    }
    Ok(found)
}

fn escape_like(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

fn path_is_within(parent: &Path, path: &Path) -> bool {
    if path.starts_with(parent) {
        return true;
    }
    let parent_key = path_key(parent).trim_end_matches('/').to_owned();
    let path_key = path_key(path);
    path_key
        .strip_prefix(&parent_key)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

fn perform_file_action(action: &FileAction) -> Result<(), String> {
    #[cfg(windows)]
    {
        run_windows_file_action(action)
    }
    #[cfg(not(windows))]
    {
        match action {
            FileAction::Move { source, target } => fs::rename(source, target)
                .map_err(|error| format!("移动 {} 失败：{error}", source.display())),
            FileAction::Recycle { source } => Err(format!(
                "当前平台不支持回收站操作，为避免永久删除未处理：{}",
                source.display()
            )),
        }
    }
}

#[cfg(windows)]
fn run_windows_file_action(action: &FileAction) -> Result<(), String> {
    use std::sync::mpsc;

    let action = action.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("mmdbridge-file-operation".to_owned())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let result = windows_file_action_sta(&action);
            let _ = sender.send(result);
        })
        .map_err(|error| format!("无法启动 Windows 文件操作线程：{error}"))?;
    receiver
        .recv()
        .map_err(|error| format!("Windows 文件操作线程意外退出：{error}"))?
}

#[cfg(windows)]
fn windows_file_action_sta(action: &FileAction) -> Result<(), String> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            UI::Shell::{
                FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_EARLYFAILURE,
                FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink,
                IShellItem,
            },
        },
        core::HSTRING,
    };

    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if init.is_err() {
        return Err(format!("COM 初始化失败：{init:?}"));
    }
    let result = unsafe {
        (|| -> Result<(), String> {
            let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)
                .map_err(|error| format!("创建 Windows 文件操作接口失败：{error}"))?;
            let flags = FOF_NOCONFIRMATION
                | FOF_NOERRORUI
                | FOF_SILENT
                | FOFX_EARLYFAILURE
                | if matches!(action, FileAction::Recycle { .. }) {
                    FOFX_RECYCLEONDELETE
                } else {
                    Default::default()
                };
            operation
                .SetOperationFlags(flags)
                .map_err(|error| format!("设置 Windows 文件操作选项失败：{error}"))?;
            match action {
                FileAction::Move { source, target } => {
                    let source_item: IShellItem = shell_item(source)?;
                    let parent = target.parent().ok_or_else(|| "目标目录无效".to_owned())?;
                    let parent_item: IShellItem = shell_item(parent)?;
                    let new_name = target
                        .file_name()
                        .ok_or_else(|| "目标名称无效".to_owned())?
                        .to_string_lossy();
                    operation
                        .MoveItem(
                            &source_item,
                            &parent_item,
                            &HSTRING::from(new_name.as_ref()),
                            None::<&IFileOperationProgressSink>,
                        )
                        .map_err(|error| format!("安排 Windows 文件移动失败：{error}"))?;
                }
                FileAction::Recycle { source } => {
                    let source_item: IShellItem = shell_item(source)?;
                    operation
                        .DeleteItem(&source_item, None::<&IFileOperationProgressSink>)
                        .map_err(|error| format!("安排回收站删除失败：{error}"))?;
                }
            }
            operation
                .PerformOperations()
                .map_err(|error| format!("执行 Windows 文件操作失败：{error}"))?;
            if operation
                .GetAnyOperationsAborted()
                .map_err(|error| format!("读取 Windows 文件操作结果失败：{error}"))?
                .as_bool()
            {
                return Err("Windows 文件操作已中止".to_owned());
            }
            Ok(())
        })()
    };
    unsafe { CoUninitialize() };
    result
}

#[cfg(windows)]
unsafe fn shell_item(path: &Path) -> Result<windows::Win32::UI::Shell::IShellItem, String> {
    use windows::{Win32::UI::Shell::SHCreateItemFromParsingName, core::HSTRING};
    let native_path = path.to_string_lossy();
    let shell_path = if let Some(unc_path) = native_path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc_path}")
    } else if let Some(drive_path) = native_path.strip_prefix(r"\\?\")
        && drive_path.as_bytes().get(1) == Some(&b':')
    {
        drive_path.to_owned()
    } else {
        native_path.to_string()
    };
    let parsing_name = HSTRING::from(shell_path.as_str());
    unsafe { SHCreateItemFromParsingName(&parsing_name, None) }.map_err(|error| {
        format!(
            "无法解析 Windows 路径 {}（Shell 路径 {}）：{error}",
            native_path, shell_path
        )
    })
}
