use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    thread,
    time::UNIX_EPOCH,
};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
    #[serde(default)]
    pub source_snapshots: Vec<AssetOperationSourceSnapshot>,
    #[serde(default)]
    pub package_snapshots: Vec<AssetOperationSourceSnapshot>,
    #[serde(default)]
    pub dependency_snapshots: Vec<AssetOperationDependencySnapshot>,
    #[serde(default)]
    pub delete_mode: Option<String>,
    #[serde(default)]
    pub delete_reason: Option<String>,
    #[serde(default)]
    pub pmx_directories: Vec<AssetOperationPmxDirectory>,
    #[serde(default)]
    pub preserved_paths: Vec<String>,
    pub warnings: Vec<String>,
    pub can_execute: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationPmxDirectory {
    pub path: String,
    pub pmx_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationSourceSnapshot {
    pub path: String,
    pub file_size: String,
    pub modified_ns: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetOperationDependencySnapshot {
    pub asset_id: String,
    pub reference: Option<String>,
    pub role: String,
    pub path: Option<String>,
    pub status: String,
    pub file_size: Option<String>,
    pub modified_ns: Option<String>,
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
    DeleteModel,
}

impl OperationKind {
    fn parse(value: &str) -> CoreResult<Self> {
        match value {
            "move" => Ok(Self::Move),
            "rename" => Ok(Self::Rename),
            "recycle" => Ok(Self::Recycle),
            "delete_model" => Ok(Self::DeleteModel),
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
            Self::DeleteModel => "delete_model",
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
    retired_format: bool,
}

#[derive(Debug, Clone)]
struct Package {
    source: PathBuf,
    target: Option<PathBuf>,
    assets: Vec<StoredAsset>,
}

#[derive(Default)]
struct ModelDeleteGroup {
    parent: Option<PathBuf>,
    selected_paths: BTreeMap<String, PathBuf>,
    pmx_paths: Vec<PathBuf>,
}

struct PreparedPlan {
    view: AssetOperationPlan,
    kind: OperationKind,
    packages: Vec<Package>,
    affected_assets: Vec<StoredAsset>,
}

#[derive(Debug)]
struct IndexedDependency {
    reference: Option<String>,
    role: Option<String>,
    path: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone)]
enum FileAction {
    Move { source: PathBuf, target: PathBuf },
    Recycle { source: PathBuf },
}

struct FileActionFailure {
    message: String,
    completed: Vec<FileAction>,
    uncertain_paths: Vec<String>,
    not_started_paths: Vec<String>,
}

pub(crate) fn plan(
    library: &Library,
    operation: &str,
    asset_ids: &[String],
    destination_parent: Option<&str>,
    new_name: Option<&str>,
) -> CoreResult<AssetOperationPlan> {
    Ok(prepare(library, operation, asset_ids, destination_parent, new_name, None)?.view)
}

fn prepare(
    library: &Library,
    operation: &str,
    asset_ids: &[String],
    destination_parent: Option<&str>,
    new_name: Option<&str>,
    own_operation: Option<&str>,
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
    if kind == OperationKind::DeleteModel {
        return prepare_model_delete(library, requested_ids, own_operation);
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
    let mut source_snapshots = Vec::<AssetOperationSourceSnapshot>::new();
    let mut package_snapshots = Vec::<AssetOperationSourceSnapshot>::new();
    let mut dependency_snapshots = Vec::<AssetOperationDependencySnapshot>::new();
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
        match package_entry_snapshots(&source) {
            Ok(snapshots) => package_snapshots.extend(snapshots),
            Err(error) => warnings.push(format!(
                "无法完整记录资产包内容，请重新扫描后再操作：{}（{error}）",
                source.display()
            )),
        }
        let package_assets = load_assets_under_package(library, &source)?;
        if package_assets.is_empty() {
            warnings.push(format!("找不到资产包中的索引记录：{}", source.display()));
        }
        if package_assets.iter().any(|asset| asset.retired_format) {
            warnings.push(format!(
                "资产包包含已停用的 X 格式记录，当前操作已禁用：{}",
                source.display()
            ));
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
            match source_file_snapshot(&asset.primary_source) {
                Ok(snapshot) if path_is_within(&source, Path::new(&snapshot.path)) => {
                    match load_indexed_primary_version(library, &asset.id)? {
                        Some((path, file_size, modified_ns))
                            if same_path(Path::new(&path), Path::new(&snapshot.path))
                                && snapshot.file_size.parse::<i64>().ok() == Some(file_size)
                                && snapshot.modified_ns.parse::<i64>().ok()
                                    == Some(modified_ns) =>
                        {
                            source_snapshots.push(snapshot);
                        }
                        Some(_) => warnings.push(format!(
                            "主文件自上次扫描后已变化，请重新扫描后再操作：{}",
                            asset.primary_source
                        )),
                        None => warnings.push(format!(
                            "索引中缺少主文件版本记录，请重新扫描后再操作：{}",
                            asset.primary_source
                        )),
                    }
                }
                Ok(_) => warnings.push(format!(
                    "索引中的主文件不在待操作资产包内：{}",
                    asset.primary_source
                )),
                Err(_) => warnings.push(format!(
                    "资产包中的主文件不存在或不可读取，请重新扫描：{}",
                    asset.primary_source
                )),
            }

            let parsed_dependencies = load_parsed_dependencies(library, &asset.id)?;
            if let Some(parsed_dependencies) = parsed_dependencies {
                for dependency in parsed_dependencies {
                    let role = dependency.role.as_deref().unwrap_or("unknown");
                    let status = dependency.status.as_deref().unwrap_or("unknown");
                    let dependency_path = dependency.path.as_deref();
                    let snapshot = dependency_file_snapshot(
                        &asset.id,
                        dependency.reference.as_deref(),
                        role,
                        dependency_path,
                        status,
                    );
                    dependency_snapshots.push(snapshot);

                    if dependency.reference.is_none() || role == "unknown" {
                        warnings.push(format!(
                            "无法确认资产依赖记录内容，请重新扫描后再操作：{}",
                            asset.primary_source
                        ));
                        continue;
                    }

                    if status == "missing"
                        && role == "shared_toon_texture"
                        && is_builtin_shared_toon(dependency.reference.as_deref())
                        && dependency_path.is_some_and(|path| {
                            !Path::new(path).exists() && path_is_within(&source, Path::new(path))
                        })
                    {
                        continue;
                    }

                    match (status, dependency_path) {
                        ("resolved", Some(path)) => match fs::canonicalize(path) {
                            Ok(canonical)
                                if canonical.is_file() && path_is_within(&source, &canonical) =>
                            {
                                dependency_paths
                                    .insert(canonical.to_string_lossy().into_owned());
                            }
                            Ok(canonical) if canonical.is_file() => warnings.push(format!(
                                "检测到解析记录中的资产包外依赖，请先收拢到包内再操作：{}（资产：{}）",
                                canonical.display(),
                                asset.name
                            )),
                            _ => warnings.push(format!(
                                "解析记录中的依赖文件已不存在，请重新扫描后再操作：{}（资产：{}）",
                                path,
                                asset.name
                            )),
                        },
                        ("missing", _) => warnings.push(format!(
                            "检测到缺失依赖，请修复引用或重新扫描：{}（资产：{}）",
                            dependency_path
                                .or(dependency.reference.as_deref())
                                .unwrap_or("未知路径"),
                            asset.name
                        )),
                        ("external", _) => warnings.push(format!(
                            "检测到资产包外依赖，请先收拢到包内再操作：{}（资产：{}）",
                            dependency_path
                                .or(dependency.reference.as_deref())
                                .unwrap_or("未知路径"),
                            asset.name
                        )),
                        (_, _) => warnings.push(format!(
                            "无法确认资产依赖状态，请重新扫描后再操作：{}（资产：{}）",
                            dependency_path
                                .or(dependency.reference.as_deref())
                                .unwrap_or("未知路径"),
                            asset.name
                        )),
                    }
                }
            } else if may_have_file_dependencies(&asset.primary_source, &asset.asset_type) {
                warnings.push(format!(
                    "无法确认资产文件依赖（缺少解析记录），请重新扫描后再操作：{}",
                    asset.primary_source
                ));
            }

            let dependency_rows = load_asset_dependencies(library, &asset.id)?;
            for dependency in dependency_rows {
                match fs::canonicalize(&dependency) {
                    Ok(canonical) if path_is_within(&source, &canonical) => {
                        dependency_paths.insert(canonical.to_string_lossy().into_owned());
                        let canonical_text = canonical.to_string_lossy().into_owned();
                        dependency_snapshots.push(dependency_file_snapshot(
                            &asset.id,
                            None,
                            "indexed",
                            Some(&canonical_text),
                            "resolved",
                        ));
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
            if has_unresolved_operation(library, &asset.id, own_operation)? {
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
    let affected_ids = affected_assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<HashSet<_>>();
    append_reverse_dependency_warnings(library, &packages, &affected_ids, &mut warnings)?;
    affected_assets.sort_by(|left, right| left.id.cmp(&right.id));
    affected_assets.dedup_by(|left, right| left.id == right.id);

    source_snapshots.sort();
    source_snapshots.dedup();
    package_snapshots.sort();
    package_snapshots.dedup();
    dependency_snapshots.sort();
    dependency_snapshots.dedup();

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
        OperationKind::DeleteModel => unreachable!("model delete plans are prepared separately"),
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
        source_snapshots,
        package_snapshots,
        dependency_snapshots,
        delete_mode: None,
        delete_reason: None,
        pmx_directories: Vec::new(),
        preserved_paths: Vec::new(),
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

fn prepare_model_delete(library: &Library, requested_ids: Vec<String>, own_operation: Option<&str>) -> CoreResult<PreparedPlan> {
    let selected_assets = requested_ids
        .iter()
        .map(|asset_id| load_asset(library, asset_id))
        .collect::<CoreResult<Vec<_>>>()?;
    let mut groups = BTreeMap::<String, ModelDeleteGroup>::new();
    for asset in selected_assets {
        if asset.asset_type != "model"
            || Path::new(&asset.primary_source)
                .extension()
                .and_then(|value| value.to_str())
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("pmx"))
        {
            return Err(CoreError::AssetOperation(format!(
                "模型删除只支持 PMX 主文件：{}",
                asset.primary_source
            )));
        }
        let root = canonical_directory(&asset.root_path)?;
        if let Some(reparse_path) = path_reparse_component(Path::new(&asset.primary_source), &root)? {
            return Err(CoreError::AssetOperation(format!(
                "PMX 路径含符号链接或重解析点，无法安全回收：{}",
                reparse_path.display()
            )));
        }
        let source = canonical_file(&asset.primary_source)?;
        if !path_is_within(&root, &source) {
            return Err(CoreError::AssetOperation(format!(
                "PMX 主文件不在登记的资产根目录内：{}",
                source.display()
            )));
        }
        let parent = source.parent().ok_or_else(|| {
            CoreError::AssetOperation(format!("无法读取 PMX 父目录：{}", source.display()))
        })?.to_path_buf();
        let group = groups.entry(path_key(&parent)).or_default();
        group.parent = Some(parent);
        group.selected_paths.insert(path_key(&source), source);
    }

    let all_roots = load_roots(library)?;
    let mut eligibility_reasons = Vec::<String>::new();
    let mut folder_eligible = true;
    for group in groups.values_mut() {
        let parent = group.parent.as_ref().expect("group has a parent");
        group.pmx_paths = enumerate_direct_pmx(parent)?;
        group.pmx_paths.sort_by_key(|path| path_key(path));
        group.pmx_paths.dedup_by(|left, right| same_path(left, right));

        let selected_paths = group.selected_paths.values().cloned().collect::<Vec<_>>();
        let selected_keys = selected_paths.iter().map(|path| path_key(path)).collect::<HashSet<_>>();
        let direct_keys = group.pmx_paths.iter().map(|path| path_key(path)).collect::<HashSet<_>>();
        let mut reasons = Vec::<String>::new();
        if selected_keys.iter().any(|path| !direct_keys.contains(path)) {
            reasons.push("所选 PMX 未能在当前目录完整枚举".to_owned());
        }
        if group.pmx_paths.len() != 1 {
            reasons.push(format!("当前目录直接包含 {} 个 PMX", group.pmx_paths.len()));
        }
        if selected_paths.len() != 1 {
            reasons.push("本批次选择了同目录中的多个 PMX".to_owned());
        }
        if all_roots.iter().any(|(_, root_path)| {
            fs::canonicalize(root_path).is_ok_and(|root| same_path(&root, parent))
        }) {
            reasons.push("PMX 位于已登记 Root 或容器目录中".to_owned());
        }
        if all_roots.iter().any(|(_, root_path)| {
            fs::canonicalize(root_path).is_ok_and(|root| path_is_within(parent, &root) && !same_path(parent, &root))
        }) {
            reasons.push("目录内包含另一个已登记 Root".to_owned());
        }
        let selected_keys = selected_paths.iter().map(|path| path_key(path)).collect::<HashSet<_>>();
        match find_other_supported_asset(parent, &selected_keys) {
            Ok(Some(path)) => reasons.push(format!("目录中还有其他 MMD 主文件：{}", path.display())),
            Ok(None) => {}
            Err(error) => reasons.push(format!("无法完整检查目录内容：{error}")),
        }
        match package_symlinks(parent) {
            Ok(paths) if !paths.is_empty() => reasons.push(format!(
                "目录含符号链接或重解析点：{}",
                paths[0].display()
            )),
            Ok(_) => {}
            Err(error) => reasons.push(format!("无法完整检查目录：{error}")),
        }

        if reasons.is_empty() {
            let mut package_assets = load_assets_under_package(library, parent)?;
            for asset in load_assets_with_primary_under_package(library, parent)? {
                if package_assets.iter().all(|existing| existing.id != asset.id) {
                    package_assets.push(asset);
                }
            }
            let selected_keys = selected_paths.iter().map(|path| path_key(path)).collect::<HashSet<_>>();
            if package_assets.iter().any(|asset| {
                asset.retired_format
                    || asset.asset_type != "model"
                    || Path::new(&asset.primary_source)
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_none_or(|extension| !extension.eq_ignore_ascii_case("pmx"))
                    || fs::canonicalize(&asset.primary_source)
                        .ok()
                        .is_none_or(|path| !selected_keys.contains(&path_key(&path)))
            }) {
                reasons.push("目录内还有其他已索引资产或退役记录".to_owned());
            }
            if package_assets.is_empty() {
                reasons.push("无法确认目录中的资产索引归属".to_owned());
            }
            match package_entry_snapshots(parent) {
                Ok(_) => {}
                Err(error) => reasons.push(format!("无法完整记录目录内容：{error}")),
            }
        }

        if !reasons.is_empty() {
            folder_eligible = false;
            for reason in reasons {
                eligibility_reasons.push(format!("{}：{reason}", parent.display()));
            }
        }
    }
    if !folder_eligible && groups.len() > 1 {
        eligibility_reasons.push("本批次统一按 PMX-only 范围执行，避免不同目录采用不同删除模式".to_owned());
    }
    eligibility_reasons.sort();
    eligibility_reasons.dedup();

    let delete_mode = if folder_eligible { "folder" } else { "pmxOnly" };
    let mut warnings = Vec::<String>::new();
    let mut packages = Vec::<Package>::new();
    let mut affected_assets = Vec::<StoredAsset>::new();
    let mut source_snapshots = Vec::<AssetOperationSourceSnapshot>::new();
    let mut package_snapshots = Vec::<AssetOperationSourceSnapshot>::new();
    let mut dependency_snapshots = Vec::<AssetOperationDependencySnapshot>::new();
    let mut dependency_paths = HashSet::<String>::new();
    let mut preserved_paths = HashSet::<String>::new();
    let mut pmx_directories = Vec::<AssetOperationPmxDirectory>::new();

    for group in groups.values() {
        let parent = group.parent.as_ref().expect("group has a parent");
        let pmx_paths = group.pmx_paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>();
        pmx_directories.push(AssetOperationPmxDirectory {
            path: parent.to_string_lossy().into_owned(),
            pmx_paths,
        });
        if delete_mode == "folder" {
            let package_assets = load_assets_under_package(library, parent)?;
            let mut complete_assets = package_assets;
            for asset in load_assets_with_primary_under_package(library, parent)? {
                if complete_assets.iter().all(|existing| existing.id != asset.id) {
                    complete_assets.push(asset);
                }
            }
            if complete_assets.is_empty() {
                warnings.push(format!("无法确认模型文件夹的索引资产：{}", parent.display()));
            }
            match package_entry_snapshots(parent) {
                Ok(snapshots) => package_snapshots.extend(snapshots),
                Err(error) => warnings.push(format!("无法完整记录模型文件夹内容：{error}")),
            }
            packages.push(Package {
                source: parent.clone(),
                target: None,
                assets: complete_assets,
            });
        } else {
            for path in group.selected_paths.values() {
                let assets = load_assets_for_primary(library, path)?;
                if assets.is_empty() {
                    warnings.push(format!("找不到所选 PMX 的索引记录：{}", path.display()));
                }
                for sibling in &group.pmx_paths {
                    if !same_path(sibling, path) {
                        preserved_paths.insert(sibling.to_string_lossy().into_owned());
                    }
                }
                packages.push(Package {
                    source: path.clone(),
                    target: None,
                    assets,
                });
            }
            preserved_paths.insert(parent.to_string_lossy().into_owned());
            for asset in load_assets_under_package(library, parent)? {
                if group.selected_paths.values().all(|path| {
                    fs::canonicalize(&asset.primary_source)
                        .ok()
                        .is_none_or(|asset_path| !same_path(&asset_path, path))
                }) {
                    preserved_paths.insert(asset.primary_source);
                }
            }
        }
    }

    for package in &packages {
        for asset in &package.assets {
            if !affected_assets.iter().any(|existing| existing.id == asset.id) {
                affected_assets.push(asset.clone());
            }
            match source_file_snapshot(&asset.primary_source) {
                Ok(snapshot) => match load_indexed_primary_version(library, &asset.id)? {
                    Some((path, file_size, modified_ns))
                        if same_path(Path::new(&path), Path::new(&snapshot.path))
                            && snapshot.file_size.parse::<i64>().ok() == Some(file_size)
                            && snapshot.modified_ns.parse::<i64>().ok() == Some(modified_ns) =>
                    {
                        source_snapshots.push(snapshot);
                    }
                    Some(_) => warnings.push(format!(
                        "主文件自上次扫描后已变化，请重新扫描后再操作：{}",
                        asset.primary_source
                    )),
                    None => warnings.push(format!(
                        "索引中缺少主文件版本记录，请重新扫描后再操作：{}",
                        asset.primary_source
                    )),
                },
                Err(_) => warnings.push(format!(
                    "主文件不存在或不可读取，请重新扫描：{}",
                    asset.primary_source
                )),
            }

            if let Some(dependencies) = load_parsed_dependencies(library, &asset.id)? {
                if delete_mode == "folder" && may_have_file_dependencies(&asset.primary_source, &asset.asset_type) && dependencies.is_empty() {
                    warnings.push(format!("无法确认资产文件依赖，请重新扫描后再操作：{}", asset.primary_source));
                }
                for dependency in dependencies {
                    let role = dependency.role.as_deref().unwrap_or("unknown");
                    let status = dependency.status.as_deref().unwrap_or("unknown");
                    let path = dependency.path.as_deref();
                    dependency_snapshots.push(dependency_file_snapshot(
                        &asset.id,
                        dependency.reference.as_deref(),
                        role,
                        path,
                        status,
                    ));
                    if delete_mode == "folder" {
                        if dependency.reference.is_none() || role == "unknown" {
                            warnings.push(format!("无法确认资产依赖记录内容，请重新扫描后再操作：{}", asset.primary_source));
                            continue;
                        }
                        if status == "missing" && role == "shared_toon_texture" && is_builtin_shared_toon(dependency.reference.as_deref()) {
                            continue;
                        }
                        match (status, path) {
                            ("resolved", Some(path)) => match fs::canonicalize(path) {
                                Ok(canonical) if canonical.is_file() && path_is_within(&package.source, &canonical) => {
                                    dependency_paths.insert(canonical.to_string_lossy().into_owned());
                                }
                                Ok(canonical) if canonical.is_file() => warnings.push(format!("存在资产文件夹外依赖，不能整目录回收：{}", canonical.display())),
                                _ => warnings.push(format!("依赖文件缺失或不可读取，请重新扫描：{path}")),
                            },
                            ("missing", _) | ("external", _) | (_, _) => warnings.push(format!(
                                "依赖状态不满足整目录回收要求：{}",
                                path.or(dependency.reference.as_deref()).unwrap_or("未知路径")
                            )),
                        }
                    }
                }
            } else if delete_mode == "folder" && may_have_file_dependencies(&asset.primary_source, &asset.asset_type) {
                warnings.push(format!("无法确认资产文件依赖（缺少解析记录），请重新扫描后再操作：{}", asset.primary_source));
            }

            for dependency in load_asset_dependencies(library, &asset.id)? {
                let dependency_text = dependency.to_string_lossy().into_owned();
                dependency_snapshots.push(dependency_file_snapshot(
                    &asset.id,
                    None,
                    "indexed",
                    Some(&dependency_text),
                    if dependency.exists() { "resolved" } else { "missing" },
                ));
                if delete_mode == "folder" {
                    match fs::canonicalize(&dependency) {
                        Ok(canonical) if path_is_within(&package.source, &canonical) => {
                            dependency_paths.insert(canonical.to_string_lossy().into_owned());
                        }
                        Ok(canonical) => warnings.push(format!("索引依赖位于模型文件夹之外：{}", canonical.display())),
                        Err(_) => warnings.push(format!("索引中的依赖文件已不存在：{}", dependency.display())),
                    }
                }
            }
            if delete_mode == "folder"
                && let Some(card_path) = load_card_path(library, &asset.id)?
                && !path_is_within(&package.source, Path::new(&card_path))
                && Path::new(&card_path).exists()
            {
                warnings.push(format!("资源卡位于模型文件夹目录之外：{card_path}"));
            }
            if has_active_job(library, &asset.id)? {
                warnings.push(format!("资产仍有后台任务运行，请等待任务完成后再操作：{}", asset.name));
            }
            if has_unresolved_operation(library, &asset.id, own_operation)? {
                warnings.push(format!("该资产有待人工核对的文件操作记录，请先恢复文件并重新扫描：{}", asset.name));
            }
        }
    }

    let affected_ids = affected_assets.iter().map(|asset| asset.id.clone()).collect::<HashSet<_>>();
    append_reverse_dependency_warnings(library, &packages, &affected_ids, &mut warnings)?;
    if !cfg!(windows) {
        warnings.push("此平台没有启用回收站接口；为避免永久删除，不能执行删除".to_owned());
    }
    affected_assets.sort_by(|left, right| left.id.cmp(&right.id));
    source_snapshots.sort();
    source_snapshots.dedup();
    package_snapshots.sort();
    package_snapshots.dedup();
    dependency_snapshots.sort();
    dependency_snapshots.dedup();
    let mut source_paths = packages.iter().map(|package| package.source.to_string_lossy().into_owned()).collect::<Vec<_>>();
    source_paths.sort();
    source_paths.dedup();
    let mut dependency_paths = dependency_paths.into_iter().collect::<Vec<_>>();
    dependency_paths.sort();
    let mut preserved_paths = preserved_paths.into_iter().collect::<Vec<_>>();
    preserved_paths.sort();
    pmx_directories.sort();
    warnings.sort();
    warnings.dedup();
    let delete_reason = (delete_mode == "pmxOnly").then(|| {
        if eligibility_reasons.is_empty() {
            "本批次按所选 PMX 文件范围执行。".to_owned()
        } else {
            format!("本次仅回收所选 PMX；不回收其所在文件夹。原因：{}。", eligibility_reasons.join("；"))
        }
    });
    let view = AssetOperationPlan {
        operation: OperationKind::DeleteModel.as_str().to_owned(),
        asset_ids: requested_ids,
        source_paths,
        destination_paths: Vec::new(),
        destination_parent: None,
        new_name: None,
        affected_assets: affected_assets.iter().map(|asset| AssetOperationAsset {
            id: asset.id.clone(),
            name: asset.name.clone(),
            primary_source: asset.primary_source.clone(),
        }).collect(),
        dependency_paths,
        source_snapshots,
        package_snapshots,
        dependency_snapshots,
        delete_mode: Some(delete_mode.to_owned()),
        delete_reason,
        pmx_directories,
        preserved_paths,
        warnings: warnings.clone(),
        can_execute: warnings.is_empty(),
    };
    Ok(PreparedPlan { view, kind: OperationKind::DeleteModel, packages, affected_assets })
}

fn enumerate_direct_pmx(parent: &Path) -> CoreResult<Vec<PathBuf>> {
    let entries = fs::read_dir(parent).map_err(|error| {
        CoreError::AssetOperation(format!("无法完整枚举模型目录 {}：{error}", parent.display()))
    })?;
    let mut pmx_paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CoreError::AssetOperation(format!("无法完整枚举模型目录 {}：{error}", parent.display()))
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("pmx")) {
            pmx_paths.push(path);
        }
    }
    Ok(pmx_paths)
}

fn find_other_supported_asset(parent: &Path, selected_paths: &HashSet<String>) -> CoreResult<Option<PathBuf>> {
    for entry in walkdir::WalkDir::new(parent).follow_links(false).min_depth(1) {
        let entry = entry.map_err(|error| CoreError::AssetOperation(format!("无法完整枚举目录内容：{error}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let supported_main_file = path.extension().and_then(|value| value.to_str()).is_some_and(|extension| {
            ["pmx", "pmd", "vmd", "vpd"].iter().any(|candidate| extension.eq_ignore_ascii_case(candidate))
        });
        if supported_main_file && !selected_paths.contains(&path_key(path)) {
            return Ok(Some(path.to_path_buf()));
        }
    }
    Ok(None)
}

fn load_assets_for_primary(library: &Library, primary: &Path) -> CoreResult<Vec<StoredAsset>> {
    let primary_text = primary.to_string_lossy().into_owned();
    let ids = {
        let connection = library.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM assets WHERE asset_type='model' AND retired_format=0 AND visibility='normal'
             AND primary_source=?1 COLLATE NOCASE ORDER BY id",
        )?;
        statement.query_map([&primary_text], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut assets = Vec::new();
    for id in ids {
        let asset = load_asset(library, &id)?;
        if fs::canonicalize(&asset.primary_source).is_ok_and(|path| same_path(&path, primary)) {
            assets.push(asset);
        }
    }
    Ok(assets)
}

fn load_assets_with_primary_under_package(library: &Library, package: &Path) -> CoreResult<Vec<StoredAsset>> {
    let package_text = package.to_string_lossy().into_owned();
    let prefix = format!("{}{}", package_text.trim_end_matches(['\\', '/']), std::path::MAIN_SEPARATOR);
    let pattern = format!("{}%", escape_like(&prefix));
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.name,a.primary_source,a.asset_directory,a.root_id,r.path,r.asset_type,a.retired_format
         FROM assets a JOIN roots r ON r.id=a.root_id
         WHERE a.primary_source=?1 COLLATE NOCASE
            OR a.primary_source LIKE ?2 ESCAPE '!' COLLATE NOCASE
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
            retired_format: row.get(7)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return metadata.file_attributes() & 0x400 != 0;
    }
    #[cfg(not(windows))]
    false
}

fn path_reparse_component(path: &Path, root: &Path) -> CoreResult<Option<PathBuf>> {
    let mut current = path.to_path_buf();
    loop {
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            CoreError::AssetOperation(format!("无法检查模型路径 {}：{error}", current.display()))
        })?;
        if is_reparse_point(&metadata) {
            return Ok(Some(current));
        }
        if same_path(&current, root) {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    Ok(None)
}

pub(crate) fn execute(
    library: &Library,
    confirmed_plan: &AssetOperationPlan,
) -> CoreResult<AssetOperationJournalEntry> {
    if !confirmed_plan.can_execute { return Err(CoreError::AssetOperation("不能执行被安全检查阻止的计划".to_owned())); }
    let _operation_guard = library.asset_operation_guard()?;
    let prepared = prepare(
        library,
        &confirmed_plan.operation,
        &confirmed_plan.asset_ids,
        confirmed_plan.destination_parent.as_deref(),
        confirmed_plan.new_name.as_deref(),
        None,
    )?;
    if !prepared.view.can_execute {
        return Err(CoreError::AssetOperation(format!(
            "操作计划不可执行：{}",
            prepared.view.warnings.join("；")
        )));
    }
    if !plans_match(&prepared.view, confirmed_plan) {
        return Err(CoreError::AssetOperation(
            "资产或文件在确认后发生变化，请重新生成操作计划".to_owned(),
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
        json!({"message":"Operation started","ownerPid":std::process::id()}),
    )?;

    let current = prepare(library, &confirmed_plan.operation, &confirmed_plan.asset_ids,
        confirmed_plan.destination_parent.as_deref(), confirmed_plan.new_name.as_deref(), Some(&operation_id));
    let prepared = match current {
        Ok(current) if current.view.can_execute && plans_match(&current.view, confirmed_plan) => current,
        result => {
            let message = match result {
                Err(error) => error.to_string(),
                Ok(_) => "资产或文件在取得操作占用前发生变化，请重新生成操作计划".to_owned(),
            };
            finish_journal(library, &operation_id, "Failed", json!({"message":message}))?;
            return Err(CoreError::AssetOperation(message));
        }
    };

    if let Some(asset) = prepared
        .affected_assets
        .iter()
        .find(|asset| has_active_job(library, &asset.id).unwrap_or(true))
    {
        let message = format!("资产新增了后台任务，文件操作未执行：{}", asset.name);
        finish_journal(library, &operation_id, "Failed", json!({"message":message}))?;
        return Err(CoreError::AssetOperation(message));
    }

    let moved_paths = match prepare_moved_paths(library, &prepared) {
        Ok(paths) => paths,
        Err(error) => {
            let message = format!("索引路径预检失败，文件操作未执行：{error}");
            finish_journal(library, &operation_id, "Failed", json!({"message":message}))?;
            return Err(CoreError::AssetOperation(message));
        }
    };
    let actions = make_actions(&prepared);
    let completed = match execute_actions(&actions) {
        Ok(completed) => completed,
        Err(failure) => {
            let status = if matches!(prepared.kind, OperationKind::Recycle | OperationKind::DeleteModel) || !failure.completed.is_empty() {
                "RecoveryNeeded"
            } else {
                "Failed"
            };
            let message = failure.message;
            let result = json!({
                "message": message.clone(),
                "completedPaths": failure.completed.iter().map(file_action_path).collect::<Vec<_>>(),
                "uncertainPaths": failure.uncertain_paths,
                "notStartedPaths": failure.not_started_paths,
            });
            finish_journal(library, &operation_id, status, result)?;
            return Err(CoreError::AssetOperation(message));
        }
    };

    let index_warnings = match apply_index_change(library, &prepared, &moved_paths) {
        Ok(warnings) => warnings,
        Err(error) => {
            if matches!(prepared.kind, OperationKind::Recycle | OperationKind::DeleteModel) {
                let message = format!(
                    "文件已发送到回收站，但索引更新失败；请从操作日志查看源路径并恢复：{error}"
                );
                let result = json!({
                    "message": message.clone(),
                    "completedPaths": completed.iter().map(file_action_path).collect::<Vec<_>>(),
                    "uncertainPaths": [],
                    "notStartedPaths": [],
                    "indexUpdateFailed": true,
                });
                finish_journal(
                    library,
                    &operation_id,
                    "RecoveryNeeded",
                    result,
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
        OperationKind::DeleteModel if prepared.view.delete_mode.as_deref() == Some("folder") => "模型文件夹已发送到 Windows 回收站".to_owned(),
        OperationKind::DeleteModel => "所选 PMX 已发送到 Windows 回收站".to_owned(),
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

fn plans_match(left: &AssetOperationPlan, right: &AssetOperationPlan) -> bool {
    left.source_paths == right.source_paths && left.destination_paths == right.destination_paths
        && left.affected_assets == right.affected_assets && left.dependency_paths == right.dependency_paths
        && left.source_snapshots == right.source_snapshots && left.package_snapshots == right.package_snapshots
        && left.dependency_snapshots == right.dependency_snapshots && left.delete_mode == right.delete_mode
        && left.delete_reason == right.delete_reason && left.pmx_directories == right.pmx_directories
        && left.preserved_paths == right.preserved_paths && left.warnings == right.warnings
}

pub(crate) fn mark_interrupted(library: &Library) -> CoreResult<()> {
    let connection = library.connection()?;
    let started = connection.prepare("SELECT id,result_json FROM operation_journal WHERE status='Started'")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, result) in started {
        let owner = serde_json::from_str::<Value>(&result).ok()
            .and_then(|value| value.get("ownerPid").and_then(Value::as_i64));
        if crate::scan_queue::claim_owner_is_live(owner) { continue; }
        connection.execute(
        "UPDATE operation_journal SET status='RecoveryNeeded',updated_at=?1,
         result_json=json_patch(CASE WHEN json_valid(result_json) THEN result_json ELSE '{}' END,
           json_object('message','程序在操作完成确认前停止；请检查源路径和目标路径，恢复文件并重新扫描后再确认处理','interruptedAt',?1))
         WHERE id=?2 AND status='Started'",
        params![Utc::now().to_rfc3339(), id],
        )?;
    }
    Ok(())
}

pub(crate) fn resolve_journal(library: &Library, id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    let changed = connection.execute(
        "UPDATE operation_journal SET status='Resolved',updated_at=?1,
         result_json=json_patch(CASE WHEN json_valid(result_json) THEN result_json ELSE '{}' END,
           json_object('message','用户已确认手动恢复并重新扫描','resolvedAt',?1))
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
            OperationKind::DeleteModel => FileAction::Recycle {
                source: package.source.clone(),
            },
        })
        .collect()
}

fn execute_actions(actions: &[FileAction]) -> Result<Vec<FileAction>, FileActionFailure> {
    let mut completed = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        if let Err(error) = perform_file_action(action) {
            let inferred_completed = match action {
                FileAction::Move { source, target } => !source.exists() && target.exists(),
                FileAction::Recycle { .. } => false,
            };
            if inferred_completed {
                completed.push(action.clone());
            }
            return Err(FileActionFailure {
                message: error,
                completed,
                uncertain_paths: if inferred_completed { Vec::new() } else { vec![file_action_path(action)] },
                not_started_paths: actions[index + 1..].iter().map(file_action_path).collect(),
            });
        }
        completed.push(action.clone());
    }
    Ok(completed)
}

fn file_action_path(action: &FileAction) -> String {
    match action {
        FileAction::Move { source, .. } | FileAction::Recycle { source } => source.to_string_lossy().into_owned(),
    }
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

fn prepare_moved_paths(library: &Library, prepared: &PreparedPlan) -> CoreResult<HashMap<String, String>> {
    let mut moved_paths = HashMap::new();
    let connection = library.connection()?;
    for package in &prepared.packages {
        let Some(target) = package.target.as_ref() else { continue; };
        for asset in &package.assets {
            let mut paths = vec![asset.primary_source.clone(), asset.asset_directory.clone()];
            let mut statement = connection.prepare("SELECT path FROM asset_files WHERE asset_id=?1")?;
            paths.extend(statement.query_map([&asset.id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?);
            if let Some(card_path) = connection.query_row(
                "SELECT card_path FROM cards WHERE asset_id=?1", [&asset.id], |row| row.get::<_, String>(0),
            ).optional()? { paths.push(card_path); }
            let parsed: Option<String> = connection.query_row(
                "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'", [&asset.id], |row| row.get(0),
            ).optional()?;
            if let Some(parsed) = parsed.and_then(|text| serde_json::from_str::<Value>(&text).ok())
                && let Some(dependencies) = parsed.get("file_dependencies").and_then(Value::as_array)
            {
                paths.extend(dependencies.iter().filter_map(|entry| entry.get("path").and_then(Value::as_str)).map(str::to_owned));
            }
            paths.extend(prepared.view.dependency_snapshots.iter()
                .filter(|entry| entry.asset_id == asset.id).filter_map(|entry| entry.path.clone()));
            for path in paths {
                // Resolve Windows short names and verbatim prefixes while the source still exists.
                let canonical = fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(&path));
                let new_path = remap_path(&canonical.to_string_lossy(), &package.source, target)?;
                moved_paths.insert(path, new_path);
            }
        }
    }
    Ok(moved_paths)
}

fn apply_index_change(library: &Library, prepared: &PreparedPlan, moved_paths: &HashMap<String, String>) -> CoreResult<Vec<String>> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction()?;
    if matches!(prepared.kind, OperationKind::Recycle | OperationKind::DeleteModel) {
        for asset in &prepared.affected_assets {
            transaction.execute("DELETE FROM assets WHERE id=?1", [&asset.id])?;
        }
        transaction.commit()?;
        drop(connection);
        return Ok(refresh_derived_indices(library));
    }

    let mapped_path = |path: &str| moved_paths.get(path).cloned().ok_or_else(|| {
        CoreError::AssetOperation(format!("索引路径在文件操作期间发生变化，已拒绝更新：{path}"))
    });
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
            let primary = mapped_path(&asset.primary_source)?;
            let asset_directory = mapped_path(&asset.asset_directory)?;
            let old_statuses: String = transaction.query_row(
                "SELECT statuses_json FROM assets WHERE id=?1",
                [&asset.id],
                |row| row.get(0),
            )?;
            let mut statuses =
                serde_json::from_str::<Vec<String>>(&old_statuses).unwrap_or_default();
            statuses.retain(|status| status != "MissingSource");
            let statuses_json = serde_json::to_string(&statuses)?;
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
                let new_path = mapped_path(&old_path)?;
                transaction.execute(
                    "UPDATE asset_files SET path=?3,path_key=?4 WHERE asset_id=?1 AND path=?2",
                    params![asset.id, old_path, new_path,
                        crate::scanner::scan_path_key(Path::new(&new_path))],
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
                let new_card_path = mapped_path(&card_path)?;
                transaction.execute(
                    "UPDATE cards SET card_path=?2,status=CASE WHEN status='CardMissing' THEN status ELSE 'CardStale' END,last_checked_at=?3 WHERE asset_id=?1",
                    params![asset.id, new_card_path, Utc::now().to_rfc3339()],
                )?;
            }
        }
    }
    rewrite_asset_dependency_paths(&transaction, &prepared.affected_assets, moved_paths)?;
    rewrite_relation_paths(&transaction, moved_paths)?;
    transaction.commit()?;
    drop(connection);
    Ok(refresh_derived_indices(library))
}

fn refresh_derived_indices(library: &Library) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Err(error) = crate::relations::rebuild(library) {
        warnings.push(format!("关系建议刷新失败：{error}"));
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

fn rewrite_asset_dependency_paths(
    transaction: &rusqlite::Transaction<'_>,
    assets: &[StoredAsset],
    moved_paths: &HashMap<String, String>,
) -> CoreResult<()> {
    for asset in assets {
        let parsed_json = transaction
            .query_row(
                "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
                [&asset.id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(parsed_json) = parsed_json else {
            continue;
        };
        let Ok(mut parsed) = serde_json::from_str::<Value>(&parsed_json) else {
            continue;
        };
        let Some(dependencies) = parsed
            .get_mut("file_dependencies")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let mut changed = false;
        for dependency in dependencies {
            let Some(path) = dependency
                .get_mut("path")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if let Some((_, new_path)) = moved_paths
                .iter()
                .find(|(old_path, _)| path_key(Path::new(old_path)) == path_key(Path::new(&path)))
                && let Some(path_value) = dependency.get_mut("path")
            {
                *path_value = Value::String(new_path.clone());
                changed = true;
            }
        }
        if changed {
            transaction.execute(
                "UPDATE metadata SET value_json=?2 WHERE asset_id=?1 AND key='parsed'",
                params![asset.id, serde_json::to_string(&parsed)?],
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
    mut result: Value,
) -> CoreResult<()> {
    let mut connection = library.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let occupied_paths = transaction.prepare(
        "SELECT sources_json,destinations_json FROM operation_journal WHERE status IN ('Started','RecoveryNeeded')",
    )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (occupied_sources, occupied_destinations) in occupied_paths {
        let occupied_sources: Vec<String> = serde_json::from_str(&occupied_sources)?;
        let occupied_destinations: Vec<String> = serde_json::from_str(&occupied_destinations)?;
        if sources.iter().chain(destinations).any(|path| occupied_sources.iter().chain(&occupied_destinations)
            .any(|occupied| same_path(Path::new(path), Path::new(occupied))
                || path_is_within(Path::new(path), Path::new(occupied))
                || path_is_within(Path::new(occupied), Path::new(path)))) {
            return Err(CoreError::AssetOperation("源路径或目标路径已有进行中或待恢复的文件操作，请先处理该操作".to_owned()));
        }
    }
    let mut root_ids = HashSet::new();
    for asset_id in asset_ids {
        let root_id: Option<String> = transaction.query_row("SELECT root_id FROM assets WHERE id=?1", [asset_id], |row| row.get(0)).optional()?;
        root_ids.insert(root_id.ok_or_else(|| CoreError::AssetNotFound(asset_id.clone()))?);
        if is_asset_operation_active_on(&transaction, asset_id)? {
            return Err(CoreError::AssetOperation("该资产已有进行中或待恢复的文件操作".to_owned()));
        }
        let active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE asset_id=?1 AND status IN ('Pending','Parsing','Rendering','Encoding','Cancelling'))",
            [asset_id], |row| row.get(0),
        )?;
        if active { return Err(CoreError::AssetOperation("该资产仍有后台任务占用，请等待任务退出后再操作".to_owned())); }
    }
    let roots = transaction.prepare("SELECT id,path FROM roots")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (root_id, root_path) in roots {
        if sources.iter().chain(destinations).any(|path| path_is_within(Path::new(&root_path), Path::new(path)) || path_is_within(Path::new(path), Path::new(&root_path))) {
            root_ids.insert(root_id);
        }
    }
    let mut root_ids = root_ids.into_iter().collect::<Vec<_>>();
    root_ids.sort();
    let active_scan: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM scan_state WHERE root_id IN (SELECT value FROM json_each(?1))
         AND status IN ('Discovering','Indexing','Verifying','Relations','Pausing','Cancelling'))",
        [serde_json::to_string(&root_ids)?], |row| row.get(0),
    )?;
    if active_scan { return Err(CoreError::AssetOperation("源目录或目标目录仍有扫描任务占用，请等待扫描退出后再操作".to_owned())); }
    result["rootIds"] = json!(root_ids);
    transaction.execute(
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
    transaction.commit()?;
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
    library.ensure_asset_visible(asset_id)?;
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT a.id,a.name,a.primary_source,a.asset_directory,a.root_id,r.path,r.asset_type,a.retired_format
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
                retired_format: row.get(7)?,
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
        "SELECT a.id,a.name,a.primary_source,a.asset_directory,a.root_id,r.path,r.asset_type,a.retired_format
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
            retired_format: row.get(7)?,
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

fn load_parsed_dependencies(
    library: &Library,
    asset_id: &str,
) -> CoreResult<Option<Vec<IndexedDependency>>> {
    let connection = library.connection()?;
    let parsed_json = connection
        .query_row(
            "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
            [asset_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(parsed_json) = parsed_json else {
        return Ok(None);
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&parsed_json) else {
        return Ok(None);
    };
    let Some(dependencies) = parsed.get("file_dependencies") else {
        return Ok(None);
    };
    let Some(dependencies) = dependencies.as_array() else {
        return Ok(Some(vec![IndexedDependency {
            reference: None,
            role: None,
            path: None,
            status: None,
        }]));
    };
    Ok(Some(
        dependencies
            .iter()
            .map(|dependency| IndexedDependency {
                reference: dependency
                    .get("reference")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                role: dependency
                    .get("role")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                path: dependency
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                status: dependency
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
            .collect(),
    ))
}

fn may_have_file_dependencies(primary_source: &str, asset_type: &str) -> bool {
    if asset_type != "model" && asset_type != "scene" {
        return false;
    }
    Path::new(primary_source)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["pmx", "pmd"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn load_indexed_primary_version(
    library: &Library,
    asset_id: &str,
) -> CoreResult<Option<(String, i64, i64)>> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT path,file_size,modified_ns FROM asset_files
             WHERE asset_id=?1 AND role='primary' LIMIT 1",
            [asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn is_builtin_shared_toon(reference: Option<&str>) -> bool {
    let Some(reference) = reference else {
        return false;
    };
    let name = reference.rsplit(['\\', '/']).next().unwrap_or(reference);
    if name != reference {
        return false;
    }
    let name = name.to_ascii_lowercase();
    (1..=10).any(|index| name == format!("toon{index:02}.bmp"))
}

fn source_file_snapshot(path: &str) -> std::io::Result<AssetOperationSourceSnapshot> {
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "source is not a file",
        ));
    }
    Ok(AssetOperationSourceSnapshot {
        path: canonical.to_string_lossy().into_owned(),
        file_size: metadata.len().to_string(),
        modified_ns: file_modified_ns(&metadata).to_string(),
    })
}

fn package_entry_snapshots(package: &Path) -> CoreResult<Vec<AssetOperationSourceSnapshot>> {
    let mut snapshots = Vec::new();
    for entry in walkdir::WalkDir::new(package).follow_links(false) {
        let entry = entry.map_err(|error| {
            CoreError::AssetOperation(format!("无法完整记录资产包内容：{error}"))
        })?;
        if entry.file_type().is_symlink()
            || (!entry.file_type().is_file() && !entry.file_type().is_dir())
        {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| {
            CoreError::AssetOperation(format!("无法读取资产包条目属性：{error}"))
        })?;
        let path = fs::canonicalize(entry.path()).map_err(|error| {
            CoreError::AssetOperation(format!("无法规范化资产包条目路径：{error}"))
        })?;
        snapshots.push(AssetOperationSourceSnapshot {
            path: path.to_string_lossy().into_owned(),
            file_size: metadata.len().to_string(),
            modified_ns: file_modified_ns(&metadata).to_string(),
        });
    }
    Ok(snapshots)
}

fn dependency_file_snapshot(
    asset_id: &str,
    reference: Option<&str>,
    role: &str,
    path: Option<&str>,
    status: &str,
) -> AssetOperationDependencySnapshot {
    let (path, file_size, modified_ns) = path.map_or((None, None, None), |path| {
        let canonical = fs::canonicalize(path).ok();
        let snapshot_path = canonical.as_deref().unwrap_or_else(|| Path::new(path));
        let metadata = fs::metadata(snapshot_path)
            .ok()
            .filter(|metadata| metadata.is_file());
        (
            Some(snapshot_path.to_string_lossy().into_owned()),
            metadata.as_ref().map(|metadata| metadata.len().to_string()),
            metadata
                .as_ref()
                .map(|metadata| file_modified_ns(metadata).to_string()),
        )
    });
    AssetOperationDependencySnapshot {
        asset_id: asset_id.to_owned(),
        reference: reference.map(str::to_owned),
        role: role.to_owned(),
        path,
        status: status.to_owned(),
        file_size,
        modified_ns,
    }
}

fn file_modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn append_reverse_dependency_warnings(
    library: &Library,
    packages: &[Package],
    affected_asset_ids: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> CoreResult<()> {
    let connection = library.connection()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.name,a.primary_source,m.value_json
         FROM assets a JOIN metadata m ON m.asset_id=a.id AND m.key='parsed'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (asset_id, asset_name, primary_source, parsed_json) = row?;
        if affected_asset_ids.contains(&asset_id) {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(&parsed_json) else {
            continue;
        };
        let Some(dependencies) = parsed.get("file_dependencies").and_then(Value::as_array) else {
            continue;
        };
        for dependency in dependencies {
            let Some(path) = dependency.get("path").and_then(Value::as_str) else {
                continue;
            };
            let dependency_path = Path::new(path);
            let resolved_path = fs::canonicalize(dependency_path).unwrap_or_else(|_| {
                if dependency_path.is_absolute() {
                    dependency_path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map(|current| current.join(dependency_path))
                        .unwrap_or_else(|_| dependency_path.to_path_buf())
                }
            });
            if packages
                .iter()
                .any(|package| path_is_within(&package.source, &resolved_path))
            {
                warnings.push(format!(
                    "其他资产引用待操作资产包中的文件，移动或删除会使其失效：{}（{}）→ {}",
                    asset_name,
                    primary_source,
                    resolved_path.display()
                ));
            }
        }
    }

    let mut statement = connection.prepare(
        "SELECT a.id,a.name,a.primary_source,f.path
         FROM assets a JOIN asset_files f ON f.asset_id=a.id
         WHERE f.role<>'primary'",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (asset_id, asset_name, primary_source, path) = row?;
        if affected_asset_ids.contains(&asset_id) {
            continue;
        }
        let dependency_path = Path::new(&path);
        let resolved_path = fs::canonicalize(dependency_path).unwrap_or_else(|_| {
            if dependency_path.is_absolute() {
                dependency_path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|current| current.join(dependency_path))
                    .unwrap_or_else(|_| dependency_path.to_path_buf())
            }
        });
        if packages
            .iter()
            .any(|package| path_is_within(&package.source, &resolved_path))
        {
            warnings.push(format!(
                "其他资产引用待操作资产包中的文件，移动或删除会使其失效：{}（{}）→ {}",
                asset_name,
                primary_source,
                resolved_path.display()
            ));
        }
    }
    Ok(())
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
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE asset_id=?1 AND status IN ('Pending','Parsing','Rendering','Encoding','Cancelling'))",
            [asset_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn has_unresolved_operation(library: &Library, asset_id: &str, own_operation: Option<&str>) -> CoreResult<bool> {
    let connection = library.connection()?;
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM operation_journal j, json_each(j.asset_ids_json) ids
               WHERE j.status IN ('Started','RecoveryNeeded') AND ids.value=?1 AND (?2 IS NULL OR j.id<>?2)
             )",
            params![asset_id, own_operation],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn is_asset_operation_active(library: &Library, asset_id: &str) -> CoreResult<bool> {
    let connection = library.connection()?;
    is_asset_operation_active_on(&connection, asset_id)
}

pub(crate) fn is_asset_operation_active_on(connection: &Connection, asset_id: &str) -> CoreResult<bool> {
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

pub(crate) fn is_root_operation_active_on(connection: &Connection, root_id: &str) -> CoreResult<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM operation_journal j WHERE j.status='Started' AND (
           EXISTS(SELECT 1 FROM json_each(j.result_json,'$.rootIds') ids WHERE ids.value=?1)
           OR EXISTS(SELECT 1 FROM json_each(j.asset_ids_json) ids JOIN assets a ON a.id=ids.value WHERE a.root_id=?1)))",
        [root_id], |row| row.get(0),
    )?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruption_and_resolution_preserve_recovery_evidence() {
        let library = Library::in_memory().unwrap();
        let evidence = json!({
            "ownerPid":2147483647, "rootIds":["root"],
            "completedPaths":["已完成"], "uncertainPaths":["待核查"],
            "notStartedPaths":["未开始"], "indexUpdateFailed":true,
        });
        library.connection().unwrap().execute(
            "INSERT INTO operation_journal(id,operation,status,asset_ids_json,sources_json,destinations_json,created_at,updated_at,result_json)
             VALUES ('recovery','move','Started','[]','[]','[]','now','now',?1)",
            [evidence.to_string()],
        ).unwrap();
        mark_interrupted(&library).unwrap();
        let interrupted = library.list_operation_journal(10).unwrap().remove(0);
        assert_eq!(interrupted.status, "RecoveryNeeded");
        for key in ["rootIds", "completedPaths", "uncertainPaths", "notStartedPaths", "indexUpdateFailed"] {
            assert_eq!(interrupted.result[key], evidence[key]);
        }
        assert!(interrupted.result["interruptedAt"].is_string());
        assert!(library.resolve_operation_journal("recovery").unwrap());
        let resolved = library.list_operation_journal(10).unwrap().remove(0);
        assert_eq!(resolved.status, "Resolved");
        for key in ["rootIds", "completedPaths", "uncertainPaths", "notStartedPaths", "indexUpdateFailed"] {
            assert_eq!(resolved.result[key], evidence[key]);
        }
        assert_eq!(resolved.result["interruptedAt"], interrupted.result["interruptedAt"]);
        assert!(resolved.result["resolvedAt"].is_string());
        assert!(!library.resolve_operation_journal("recovery").unwrap());
        assert_eq!(library.list_operation_journal(10).unwrap()[0].result, resolved.result);
    }

    #[test]
    fn resolution_cannot_release_a_live_operation_or_rewrite_terminal_entries() {
        let library = Library::in_memory().unwrap();
        for (id, status) in [("live", "Started"), ("done", "Completed"), ("failed", "Failed"), ("recover", "RecoveryNeeded")] {
            library.connection().unwrap().execute(
                "INSERT INTO operation_journal(id,operation,status,asset_ids_json,sources_json,destinations_json,created_at,updated_at,result_json)
                 VALUES (?1,'move',?2,?3,'[]','[]','now','now',?4)",
                params![id, status, json!([id]).to_string(), json!({"ownerPid":std::process::id(),"marker":id}).to_string()],
            ).unwrap();
        }
        assert!(is_asset_operation_active_on(&library.connection().unwrap(), "live").unwrap());
        for id in ["live", "done", "failed"] {
            assert!(!library.resolve_operation_journal(id).unwrap());
        }
        assert!(is_asset_operation_active_on(&library.connection().unwrap(), "live").unwrap());
        assert!(is_asset_operation_active_on(&library.connection().unwrap(), "recover").unwrap());
        assert!(library.resolve_operation_journal("recover").unwrap());
        assert!(!is_asset_operation_active_on(&library.connection().unwrap(), "recover").unwrap());
        let entries = library.list_operation_journal(10).unwrap();
        for (id, status) in [("live", "Started"), ("done", "Completed"), ("failed", "Failed")] {
            let entry = entries.iter().find(|entry| entry.id == id).unwrap();
            assert_eq!(entry.status, status);
            assert_eq!(entry.result["marker"], id);
        }
    }

    #[test]
    fn actual_move_and_rename_preserve_package_identity_annotations_and_dependencies() {
        let fixture = Fixture::new();
        let library = Library::in_memory().unwrap();
        let (source_root, destination_root) = setup_roots(&fixture, &library);
        let package = source_root.join("模型_日本");
        fs::create_dir_all(&package).unwrap();
        let primary = package.join("model.pmx");
        let texture = package.join("texture.png");
        fs::write(&primary, b"fixture-pmx").unwrap();
        fs::write(&texture, b"texture").unwrap();
        fs::write(package.join("说明.txt"), b"package-note").unwrap();
        let parsed = json!({
            "file_type":"pmx",
            "file_dependencies":[dependency("texture.png", "texture", &texture, "resolved")],
        });
        add_asset(&library, "asset-1", "source-root", "Fixture", &primary, &package, &parsed, &[(texture.to_str().unwrap(), "texture")]);
        library.add_asset_tag("asset-1", "手动标签", "user", None).unwrap();
        library.set_favorite("asset-1", true).unwrap();

        let plan = move_plan(&library, &destination_root);
        assert!(plan.can_execute, "{:?}", plan.warnings);
        let moved = library.execute_asset_operation(&plan).unwrap();
        assert_eq!(moved.status, "Completed");
        assert!(!package.exists());
        let moved_package = destination_root.join("模型_日本");
        assert_eq!(fs::read(moved_package.join("说明.txt")).unwrap(), b"package-note");
        let asset = library.inspect_asset("asset-1").unwrap();
        assert_eq!(asset.id, "asset-1");
        assert_eq!(asset.root_id, "destination-root");
        assert_eq!(Path::new(&asset.primary_source), fs::canonicalize(moved_package.join("model.pmx")).unwrap());
        assert!(asset.is_favorite);
        assert!(library.list_asset_tags("asset-1").unwrap().iter().any(|tag| tag.name == "手动标签"));

        let plan = library.plan_asset_operation("rename", &["asset-1".to_owned()], None, Some("重命名_日本")).unwrap();
        assert!(plan.can_execute, "{:?}", plan.warnings);
        assert_eq!(library.execute_asset_operation(&plan).unwrap().status, "Completed");
        let renamed = destination_root.join("重命名_日本");
        assert!(!moved_package.exists());
        assert_eq!(fs::read(renamed.join("model.pmx")).unwrap(), b"fixture-pmx");
        assert_eq!(fs::read(renamed.join("texture.png")).unwrap(), b"texture");
        assert_eq!(fs::read(renamed.join("说明.txt")).unwrap(), b"package-note");
        let asset = library.inspect_asset("asset-1").unwrap();
        assert_eq!(asset.id, "asset-1");
        assert!(asset.is_favorite);
        assert_eq!(Path::new(&asset.primary_source), fs::canonicalize(renamed.join("model.pmx")).unwrap());
        assert_eq!(asset.metadata["file_dependencies"][0]["path"], fs::canonicalize(renamed.join("texture.png")).unwrap().to_string_lossy().as_ref());
        assert!(library.list_asset_tags("asset-1").unwrap().iter().any(|tag| tag.name == "手动标签"));
        assert!(!is_asset_operation_active_on(&library.connection().unwrap(), "asset-1").unwrap());
        assert_eq!(library.list_operation_journal(10).unwrap().len(), 2);
    }

    #[test]
    fn journal_recovery_does_not_interrupt_a_live_operation() {
        let library = Library::in_memory().unwrap();
        let connection = library.connection().unwrap();
        for (id, owner) in [("live", i64::from(std::process::id())), ("dead", 2147483647)] {
            connection.execute("INSERT INTO operation_journal(id,operation,status,asset_ids_json,sources_json,destinations_json,created_at,updated_at,result_json)
                VALUES (?1,'move','Started','[]','[]','[]','now','now',?2)", params![id, json!({"ownerPid":owner}).to_string()]).unwrap();
        }
        drop(connection);
        mark_interrupted(&library).unwrap();
        let entries = list_journal(&library, 10).unwrap();
        assert_eq!(entries.iter().find(|entry| entry.id == "live").unwrap().status, "Started");
        assert_eq!(entries.iter().find(|entry| entry.id == "dead").unwrap().status, "RecoveryNeeded");
    }

    #[test]
    fn journal_reservation_blocks_cancelling_tasks_and_overlapping_operations() {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at) VALUES ('r','model','/r','/r','Root','now');
             INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,created_at,updated_at,last_seen_at)
             VALUES ('a','r','model','A','/r/a/model.pmx','/r/a','now','now','now');
             INSERT INTO jobs(id,asset_id,kind,status,created_at,updated_at) VALUES ('j','a','thumbnail','Cancelling','now','now');"
        ).unwrap();
        let reserve = |id| insert_journal(&library, id, OperationKind::Move, "Started", &["a".to_owned()], &[], &[], "now", json!({"ownerPid":std::process::id()}));
        assert!(reserve("first").is_err());
        assert!(library.remove_root("r").is_err());
        library.connection().unwrap().execute("UPDATE jobs SET status='Cancelled' WHERE id='j'", []).unwrap();
        library.connection().unwrap().execute("INSERT INTO scan_state(root_id,status,updated_at) VALUES ('r','Discovering','now')", []).unwrap();
        assert!(reserve("first").is_err());
        library.connection().unwrap().execute("UPDATE scan_state SET status='Completed' WHERE root_id='r'", []).unwrap();
        reserve("first").unwrap();
        assert!(library.remove_root("r").is_err());
        library.connection().unwrap().execute("UPDATE scan_state SET status='Pending' WHERE root_id='r'", []).unwrap();
        assert!(library.claim_pending_scan("r").unwrap().is_none());
        assert!(reserve("second").is_err());
        assert!(matches!(library.create_card("a", None), Err(CoreError::AssetOperation(_))));
        assert_eq!(list_journal(&library, 10).unwrap().len(), 1);
        finish_journal(&library, "first", "Completed", json!({})).unwrap();
        assert!(library.claim_pending_scan("r").unwrap().is_some());
    }

    #[test]
    fn different_assets_cannot_reserve_overlapping_destination_paths() {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at) VALUES ('r','model','/r','/r','Root','now');
             INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,created_at,updated_at,last_seen_at)
             VALUES ('a','r','model','A','/r/a/model.pmx','/r/a','now','now','now'),
                    ('b','r','model','B','/r/b/model.pmx','/r/b','now','now','now');"
        ).unwrap();
        let reserve = |id: &str, asset: &str, destination: &str| insert_journal(&library, id, OperationKind::Move, "Started",
            &[asset.to_owned()], &[format!("/r/{asset}")], &[destination.to_owned()], "now", json!({}));
        reserve("first", "a", "/target/shared").unwrap();
        assert!(reserve("second", "b", "/TARGET/shared").is_err());
        assert!(reserve("second", "b", "/target/shared/child").is_err());
        assert!(reserve("second", "b", "/target").is_err());
        reserve("second", "b", "/target/sibling").unwrap();
    }

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("mmdbridge-operation-plan-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn child(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn add_root(library: &Library, id: &str, path: &Path) {
        let path = fs::canonicalize(path).unwrap();
        let connection = library.connection().unwrap();
        connection
            .execute(
                "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at)
                 VALUES (?1,'model',?2,?3,?1,'2026-09-27T00:00:00Z')",
                params![id, path.to_string_lossy(), path_key(&path)],
            )
            .unwrap();
    }

    fn add_asset(
        library: &Library,
        id: &str,
        root_id: &str,
        name: &str,
        primary: &Path,
        package: &Path,
        parsed: &Value,
        indexed_dependencies: &[(&str, &str)],
    ) {
        let now = "2026-09-27T00:00:00Z";
        let metadata = fs::metadata(primary).unwrap();
        let primary = fs::canonicalize(primary).unwrap();
        let package = fs::canonicalize(package).unwrap();
        let connection = library.connection().unwrap();
        connection
            .execute(
                "INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,statuses_json,created_at,updated_at,last_seen_at)
                 VALUES (?1,?2,'model',?3,?4,?5,'fixture','[]',?6,?6,?6)",
                params![
                    id,
                    root_id,
                    name,
                    primary.to_string_lossy(),
                    package.to_string_lossy(),
                    now
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO asset_files(asset_id,path,path_key,role,file_size,modified_ns) VALUES (?1,?2,?3,'primary',?4,?5)",
                params![
                    id,
                    primary.to_string_lossy(),
                    crate::scanner::scan_path_key(&primary),
                    i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                    file_modified_ns(&metadata)
                ],
            )
            .unwrap();
        for (path, role) in indexed_dependencies {
            let metadata = fs::metadata(path).unwrap();
            connection
                .execute(
                    "INSERT INTO asset_files(asset_id,path,path_key,role,file_size,modified_ns) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        path,
                        crate::scanner::scan_path_key(Path::new(path)),
                        role,
                        i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                        file_modified_ns(&metadata)
                    ],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO metadata(asset_id,key,value_json) VALUES (?1,'parsed',?2)",
                params![id, parsed.to_string()],
            )
            .unwrap();
    }

    fn setup_roots(fixture: &Fixture, library: &Library) -> (PathBuf, PathBuf) {
        let source_root = fixture.child("source-root");
        let destination_root = fixture.child("destination-root");
        fs::create_dir_all(&source_root).unwrap();
        fs::create_dir_all(&destination_root).unwrap();
        add_root(library, "source-root", &source_root);
        add_root(library, "destination-root", &destination_root);
        (source_root, destination_root)
    }

    fn dependency(reference: &str, role: &str, path: &Path, status: &str) -> Value {
        let path = fs::canonicalize(path).unwrap_or_else(|_| {
            match (
                path.parent()
                    .and_then(|parent| fs::canonicalize(parent).ok()),
                path.file_name(),
            ) {
                (Some(parent), Some(file_name)) => parent.join(file_name),
                _ => path.to_path_buf(),
            }
        });
        serde_json::json!({
            "reference": reference,
            "role": role,
            "path": path.to_string_lossy(),
            "status": status,
        })
    }

    fn move_plan(library: &Library, destination_root: &Path) -> AssetOperationPlan {
        library
            .plan_asset_operation(
                "move",
                &["asset-1".to_owned()],
                destination_root.to_str(),
                None,
            )
            .unwrap()
    }

    #[test]
    fn plan_blocks_external_and_missing_dependencies_without_executing() {
        let fixture = Fixture::new();
        let library = Library::in_memory().unwrap();
        let (source_root, destination_root) = setup_roots(&fixture, &library);
        let package = source_root.join("model-package");
        let external_dir = fixture.child("external");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&external_dir).unwrap();
        let primary = package.join("model.pmx");
        let external = external_dir.join("shared.png");
        let missing = package.join("missing.png");
        fs::write(&primary, b"fixture-pmx").unwrap();
        fs::write(&external, b"texture").unwrap();
        let parsed = serde_json::json!({
            "file_type": "pmx",
            "file_dependencies": [
                dependency("../external/shared.png", "texture", &external, "external"),
                dependency("missing.png", "texture", &missing, "missing"),
                { "reference": "opaque.bin", "role": "texture", "status": "unresolved" }
            ]
        });
        add_asset(
            &library,
            "asset-1",
            "source-root",
            "Fixture Model",
            &primary,
            &package,
            &parsed,
            &[],
        );

        let plan = move_plan(&library, &destination_root);

        assert!(!plan.can_execute);
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("资产包外依赖")),
            "warnings: {:?}",
            plan.warnings
        );
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("缺失依赖")),
            "warnings: {:?}",
            plan.warnings
        );
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.contains("无法确认资产依赖状态"))
        );
        assert_eq!(plan.source_snapshots.len(), 1);
        assert!(!plan.package_snapshots.is_empty());
        assert_eq!(plan.dependency_snapshots.len(), 3);
    }

    #[test]
    fn plan_blocks_reverse_references_from_other_assets() {
        let fixture = Fixture::new();
        let library = Library::in_memory().unwrap();
        let (source_root, destination_root) = setup_roots(&fixture, &library);
        let package = source_root.join("model-package");
        let consumer_root = fixture.child("consumer-root");
        let consumer_package = consumer_root.join("consumer-package");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&consumer_package).unwrap();
        fs::create_dir_all(&consumer_root).unwrap();
        add_root(&library, "consumer-root", &consumer_root);

        let primary = package.join("model.pmx");
        let texture = package.join("shared.png");
        fs::write(&primary, b"fixture-pmx").unwrap();
        fs::write(&texture, b"texture").unwrap();
        let selected_parsed = serde_json::json!({
            "file_type": "pmx",
            "file_dependencies": [dependency("shared.png", "texture", &texture, "resolved")]
        });
        add_asset(
            &library,
            "asset-1",
            "source-root",
            "Fixture Model",
            &primary,
            &package,
            &selected_parsed,
            &[(texture.to_str().unwrap(), "texture")],
        );

        let consumer_primary = consumer_package.join("consumer.pmx");
        fs::write(&consumer_primary, b"consumer-pmx").unwrap();
        let consumer_parsed = serde_json::json!({ "file_type": "pmx" });
        add_asset(
            &library,
            "consumer-asset",
            "consumer-root",
            "External Consumer",
            &consumer_primary,
            &consumer_package,
            &consumer_parsed,
            &[(texture.to_str().unwrap(), "texture")],
        );
        library
            .connection()
            .unwrap()
            .execute(
                "UPDATE metadata SET value_json='{invalid' WHERE asset_id='consumer-asset' AND key='parsed'",
                [],
            )
            .unwrap();

        let plan = move_plan(&library, &destination_root);

        assert!(!plan.can_execute);
        assert!(
            plan.warnings.iter().any(|warning| {
                warning.contains("其他资产引用")
                    && warning.contains("External Consumer")
                    && warning.contains(
                        &fs::canonicalize(&texture)
                            .unwrap()
                            .to_string_lossy()
                            .to_string(),
                    )
            }),
            "warnings: {:?}",
            plan.warnings
        );
    }

    #[test]
    fn plan_allows_missing_standard_shared_toon_supplied_by_renderer() {
        let fixture = Fixture::new();
        let library = Library::in_memory().unwrap();
        let (source_root, destination_root) = setup_roots(&fixture, &library);
        let package = source_root.join("model-package");
        fs::create_dir_all(&package).unwrap();
        let primary = package.join("model.pmd");
        let builtin_toon = package.join("toon01.bmp");
        fs::write(&primary, b"fixture-pmd").unwrap();
        let parsed = serde_json::json!({
            "file_type": "pmd",
            "file_dependencies": [dependency(
                "toon01.bmp",
                "shared_toon_texture",
                &builtin_toon,
                "missing"
            )]
        });
        add_asset(
            &library,
            "asset-1",
            "source-root",
            "Fixture PMD",
            &primary,
            &package,
            &parsed,
            &[],
        );

        let plan = move_plan(&library, &destination_root);

        assert!(plan.can_execute, "warnings: {:?}", plan.warnings);
    }

    #[test]
    fn plan_rejects_stale_primary_and_snapshots_package_entries() {
        let fixture = Fixture::new();
        let library = Library::in_memory().unwrap();
        let (source_root, destination_root) = setup_roots(&fixture, &library);
        let package = source_root.join("model-package");
        fs::create_dir_all(&package).unwrap();
        let primary = package.join("model.pmx");
        let texture = package.join("texture.png");
        fs::write(&primary, b"fixture-pmx").unwrap();
        fs::write(&texture, b"texture").unwrap();
        let parsed = serde_json::json!({
            "file_type": "pmx",
            "file_dependencies": [dependency("texture.png", "texture", &texture, "resolved")]
        });
        add_asset(
            &library,
            "asset-1",
            "source-root",
            "Fixture Model",
            &primary,
            &package,
            &parsed,
            &[(texture.to_str().unwrap(), "texture")],
        );

        let initial = move_plan(&library, &destination_root);
        assert!(initial.can_execute, "warnings: {:?}", initial.warnings);
        fs::write(&texture, b"texture-updated").unwrap();
        let changed_dependency = move_plan(&library, &destination_root);

        assert!(changed_dependency.can_execute);
        assert_ne!(initial.dependency_snapshots, changed_dependency.dependency_snapshots);
        assert_ne!(initial.package_snapshots, changed_dependency.package_snapshots);

        fs::write(package.join("unindexed-note.txt"), b"new package file").unwrap();
        let changed_package = move_plan(&library, &destination_root);
        assert!(changed_package.can_execute);
        assert_ne!(changed_dependency.package_snapshots, changed_package.package_snapshots);

        fs::write(&primary, b"fixture-pmx-updated").unwrap();
        let changed_primary = move_plan(&library, &destination_root);

        assert!(!changed_primary.can_execute);
        assert!(changed_primary.warnings.iter().any(|warning| {
            warning.contains("主文件自上次扫描后已变化")
        }));
    }
}
