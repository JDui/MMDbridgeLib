use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    CoreError, CoreResult, Library,
    thumbnail::ThumbnailRenderReport,
    types::{Asset, AssetType, CardResult, CardValidation},
};

const CARD_FORMAT: &str = "MMDRCV";
const CARD_SCHEMA_VERSION: u32 = 1;
const MANIFEST_REVISION: &str = concat!("card-", env!("CARGO_PKG_VERSION"), "-", "1");
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PREVIEW_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestSource {
    primary_file: String,
    relative_path: String,
    fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestThumbnail {
    file: String,
    width: u32,
    height: u32,
    format: String,
    quality: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    render_report: Option<ThumbnailRenderReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestGenerator {
    name: String,
    version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CardManifest {
    format: String,
    schema_version: u32,
    asset_id: String,
    asset_type: AssetType,
    name: String,
    source: ManifestSource,
    metadata: Value,
    tags: Vec<String>,
    #[serde(default)]
    suppressed_tags: Vec<String>,
    #[serde(default)]
    favorite: bool,
    thumbnail: Option<ManifestThumbnail>,
    created_at: String,
    updated_at: String,
    generator: ManifestGenerator,
}

struct AssetContext {
    asset_id: String,
    asset_type: AssetType,
    name: String,
    source_path: PathBuf,
    root_path: PathBuf,
    fingerprint: String,
    metadata: Value,
    tags: Vec<String>,
    suppressed_tags: Vec<String>,
    is_favorite: bool,
    current_card_path: Option<PathBuf>,
    current_manifest_json: Option<String>,
}

struct ReadCard {
    manifest: CardManifest,
    manifest_json: String,
    preview_webp: Option<Vec<u8>>,
}

impl ReadCard {
    fn has_thumbnail(&self) -> bool {
        self.preview_webp.is_some()
    }
}

enum TargetKind {
    New,
    Replace,
}

pub(crate) struct CardIdentityHint {
    pub asset_id: String,
    pub card_path: PathBuf,
    pub tags: Vec<String>,
    pub suppressed_tags: Vec<String>,
    pub favorite: bool,
}

pub(crate) enum NearbyCardIdentity {
    None,
    Unique(CardIdentityHint),
    Ambiguous,
}

pub(crate) fn find_nearby_identity(
    source_path: &Path,
    display_name: &str,
    asset_type: AssetType,
    relative_path: &str,
    fingerprint: &str,
) -> NearbyCardIdentity {
    let Some(parent) = source_path.parent() else {
        return NearbyCardIdentity::None;
    };
    let safe_name = safe_filename(display_name);
    let source_name = safe_filename(
        source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default(),
    );
    let accepted_bases = [safe_name, source_name];
    let mut candidates = Vec::new();
    let Ok(entries) = fs::read_dir(parent) else {
        return NearbyCardIdentity::None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("MMDRCV"))
        {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let name_matches = accepted_bases.iter().any(|base| {
            let stem_lower = stem.to_ascii_lowercase();
            let base_lower = base.to_ascii_lowercase();
            stem_lower == base_lower
                || stem_lower
                    .strip_prefix(&base_lower)
                    .and_then(|rest| rest.strip_prefix("__"))
                    .is_some_and(|suffix| {
                        !suffix.is_empty()
                            && suffix
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
                    })
        });
        if !name_matches {
            continue;
        }
        let card = match read_card(&path) {
            Ok(card) => card,
            Err(_) => continue,
        };
        push_identity_candidate(
            &mut candidates,
            path,
            card,
            asset_type,
            relative_path,
            fingerprint,
            &file_name(source_path),
        );
    }

    if candidates.is_empty() {
        return NearbyCardIdentity::None;
    }
    let has_exact_path = candidates.iter().any(|(_, _, exact, _, _, _)| *exact);
    if has_exact_path {
        candidates.retain(|(_, _, exact, _, _, _)| *exact);
    }
    let mut ids = candidates
        .iter()
        .map(|(asset_id, _, _, _, _, _)| asset_id.as_str())
        .collect::<HashSet<_>>();
    if ids.len() != 1 {
        return NearbyCardIdentity::Ambiguous;
    }
    let asset_id = ids.drain().next().unwrap_or_default().to_owned();
    let selected = candidates
        .iter()
        .find(|(candidate_id, _, _, _, _, _)| *candidate_id == asset_id)
        .expect("the unique asset ID came from candidates");
    let card_path = selected.1.clone();
    let tags = selected.3.clone();
    let suppressed_tags = selected.4.clone();
    let favorite = selected.5;
    NearbyCardIdentity::Unique(CardIdentityHint {
        asset_id,
        card_path,
        tags,
        suppressed_tags,
        favorite,
    })
}

fn push_identity_candidate(
    candidates: &mut Vec<(String, PathBuf, bool, Vec<String>, Vec<String>, bool)>,
    path: PathBuf,
    card: ReadCard,
    asset_type: AssetType,
    relative_path: &str,
    fingerprint: &str,
    primary_file: &str,
) {
    if card.manifest.asset_type != asset_type
        || !card
            .manifest
            .source
            .primary_file
            .eq_ignore_ascii_case(primary_file)
    {
        return;
    }
    let exact_path = card
        .manifest
        .source
        .relative_path
        .eq_ignore_ascii_case(relative_path);
    let fingerprint_matches = card.manifest.source.fingerprint == fingerprint;
    if exact_path || fingerprint_matches {
        candidates.push((
            card.manifest.asset_id,
            path,
            exact_path,
            card.manifest.tags,
            card.manifest.suppressed_tags,
            card.manifest.favorite,
        ));
    }
}

pub(crate) fn verify(library: &Library, asset_id: &str) -> CoreResult<CardValidation> {
    let context = load_context(library, asset_id)?;
    let renderer_revision = expected_renderer_revision(library, context.asset_type)?;
    verify_with_renderer_revision(library, asset_id, &renderer_revision)
}

pub(crate) fn verify_with_renderer_revision(
    library: &Library,
    asset_id: &str,
    renderer_revision: &str,
) -> CoreResult<CardValidation> {
    let context = load_context(library, asset_id)?;
    let was_stale = library
        .connection()?
        .query_row(
            "SELECT status FROM cards WHERE asset_id=?1",
            [asset_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .as_deref()
        == Some("CardStale");
    let stored_card_path = context
        .current_card_path
        .clone()
        .unwrap_or_else(|| default_card_path(&context.source_path, &context.name));
    let safe_default = default_card_path(&context.source_path, &context.name);
    let preferred_path = if card_owned_by(&stored_card_path, &context.asset_id) {
        Some(stored_card_path.clone())
    } else if card_owned_by(&safe_default, &context.asset_id) {
        Some(safe_default.clone())
    } else {
        find_owned_neighbor(&context.source_path, &context.asset_id)
    };
    let card_path = preferred_path.unwrap_or_else(|| {
        if stored_card_path.exists() || !safe_default.exists() {
            stored_card_path
        } else {
            safe_default
        }
    });
    let card_was_associated = context
        .current_manifest_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<CardManifest>(json).ok())
        .is_some_and(|manifest| manifest.asset_id == context.asset_id);
    let now = Utc::now().to_rfc3339();
    let (status, manifest_json, has_thumbnail, message) = if !card_path.exists() {
        (
            "CardMissing",
            None,
            false,
            Some("资源卡文件不存在".to_owned()),
        )
    } else if !card_path.is_file() {
        (
            "CardBroken",
            None,
            false,
            Some("资源卡路径不是普通文件".to_owned()),
        )
    } else {
        match read_card(&card_path) {
            Ok(card) => {
                if card.manifest.asset_id != context.asset_id {
                    if card_was_associated {
                        (
                            "CardBroken",
                            Some(card.manifest_json.clone()),
                            card.has_thumbnail(),
                            Some("已关联的资源卡被其他资产覆盖".to_owned()),
                        )
                    } else {
                        (
                            "CardMissing",
                            None,
                            false,
                            Some(
                                "同名资源卡属于另一资产，将为当前资产使用带 ID 的文件名".to_owned(),
                            ),
                        )
                    }
                } else if card.manifest.asset_type != context.asset_type {
                    (
                        "CardBroken",
                        Some(card.manifest_json.clone()),
                        card.has_thumbnail(),
                        Some("资源卡中的 asset_type 与当前资产不一致".to_owned()),
                    )
                } else if card.manifest.source.fingerprint != context.fingerprint
                    || !path_text_equal(
                        &card.manifest.source.primary_file,
                        &file_name(&context.source_path),
                    )
                    || !path_text_equal(
                        &card.manifest.source.relative_path,
                        &relative_source(&context),
                    )
                    || card.manifest.name != context.name
                    || card.manifest.metadata != context.metadata
                    || card.manifest.tags != context.tags
                    || card.manifest.suppressed_tags != context.suppressed_tags
                    || card.manifest.favorite != context.is_favorite
                {
                    (
                        "CardStale",
                        Some(card.manifest_json.clone()),
                        card.has_thumbnail(),
                        Some("源文件、资产元数据或标签已变化，需要刷新资源卡".to_owned()),
                    )
                } else if card.manifest.generator.name != "MMDbridgeLib"
                    || card.manifest.generator.version != env!("CARGO_PKG_VERSION")
                {
                    (
                        "CardStale",
                        Some(card.manifest_json.clone()),
                        card.has_thumbnail(),
                        Some("资源卡由旧版软件生成，需要刷新".to_owned()),
                    )
                } else if card.has_thumbnail() && !thumbnail_version_current(library, &context, &card)? {
                    (
                        "CardStale",
                        Some(card.manifest_json.clone()),
                        false,
                        Some("缩略图由旧版渲染设置生成，需要刷新资源卡".to_owned()),
                    )
                } else if was_stale {
                    (
                        "CardStale",
                        Some(card.manifest_json.clone()),
                        card.has_thumbnail(),
                        Some("资产依赖或预览输入已变化，需要刷新资源卡".to_owned()),
                    )
                } else if !card.has_thumbnail() {
                    (
                        "CardValid",
                        Some(card.manifest_json),
                        false,
                        Some("资源卡有效；尚无 preview.webp，缩略图待生成".to_owned()),
                    )
                } else {
                    ("CardValid", Some(card.manifest_json), true, None)
                }
            }
            Err(error) => (
                "CardBroken",
                None,
                false,
                Some(format!("资源卡无法验证：{error}")),
            ),
        }
    };

    let file_signature = std::fs::metadata(&card_path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| {
            (
                i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&metadata),
            )
        });
    let stored_renderer_revision = manifest_renderer_revision(
        manifest_json.as_deref(),
        renderer_revision,
    );
    let connection = library.connection()?;
    connection.execute(
        "INSERT INTO cards(asset_id,card_path,status,manifest_json,last_checked_at,file_size,modified_ns,manifest_revision,renderer_revision,has_thumbnail)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(asset_id) DO UPDATE SET card_path=excluded.card_path,status=excluded.status,
           manifest_json=excluded.manifest_json,last_checked_at=excluded.last_checked_at,
           file_size=excluded.file_size,modified_ns=excluded.modified_ns,
           manifest_revision=excluded.manifest_revision,renderer_revision=excluded.renderer_revision,
           has_thumbnail=excluded.has_thumbnail",
        params![
            context.asset_id,
            card_path.to_string_lossy().into_owned(),
            status,
            manifest_json,
            now,
            file_signature.map(|signature| signature.0),
            file_signature.map(|signature| signature.1),
            MANIFEST_REVISION,
            stored_renderer_revision,
            i64::from(has_thumbnail),
        ],
    )?;

    Ok(CardValidation {
        asset_id: context.asset_id,
        status: status.to_owned(),
        card_path: Some(card_path.to_string_lossy().into_owned()),
        has_thumbnail,
        message,
    })
}

pub(crate) fn verify_if_changed(
    library: &Library,
    asset_id: &str,
    expected_renderer_revision: &str,
) -> CoreResult<Option<CardValidation>> {
    library.ensure_asset_visible(asset_id)?;
    let cached: Option<(Option<String>, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>, bool)> =
        library
            .connection()?
            .query_row(
                "SELECT c.card_path,c.status,c.file_size,c.modified_ns,c.manifest_revision,c.renderer_revision,c.has_thumbnail
                 FROM assets a LEFT JOIN cards c ON c.asset_id=a.id WHERE a.id=?1",
                [asset_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                    ))
                },
            )
            .optional()?;
    let Some((card_path, status, file_size, modified_ns, Some(manifest_revision), Some(renderer_revision), has_thumbnail)) = cached
    else {
        return Ok(None);
    };
    let Some(status) = status else { return Ok(None); };
    if manifest_revision != MANIFEST_REVISION {
        return Ok(None);
    }
    let signature_matches = if let Some(card_path) = card_path.as_deref() {
        std::fs::metadata(PathBuf::from(card_path))
            .ok()
            .filter(|metadata| metadata.is_file())
            .is_some_and(|metadata| {
                file_size == Some(i64::try_from(metadata.len()).unwrap_or(i64::MAX))
                    && modified_ns == Some(metadata_modified_ns(&metadata))
            })
    } else {
        false
    };
    let missing_matches = card_path.as_deref().is_some_and(|path| {
        std::fs::metadata(PathBuf::from(path)).is_err()
            && status == "CardMissing"
            && file_size.is_none()
            && modified_ns.is_none()
    });
    let revision_matches = renderer_revision == expected_renderer_revision
        || matches!(status.as_str(), "CardStale" | "CardBroken" | "CardMissing");
    if !revision_matches || !(signature_matches || missing_matches) {
        return Ok(None);
    }
    match status.as_str() {
        "CardValid" | "CardStale" | "CardBroken" | "CardMissing" => {
            Ok(Some(CardValidation {
                asset_id: asset_id.to_owned(),
                status,
                card_path,
                has_thumbnail,
                message: None,
            }))
        }
        _ => Ok(None),
    }
}

fn manifest_renderer_revision(manifest_json: Option<&str>, expected: &str) -> String {
    let Some(manifest) = manifest_json.and_then(|json| serde_json::from_str::<Value>(json).ok()) else {
        return expected.to_owned();
    };
    let Some(thumbnail) = manifest.get("thumbnail").filter(|value| !value.is_null()) else {
        return expected.to_owned();
    };
    let Some(report) = thumbnail.get("render_report") else {
        return "unverified".to_owned();
    };
    match (
        report.get("rendererVersion").and_then(Value::as_str),
        report.get("previewSettingsVersion").and_then(Value::as_str),
    ) {
        (Some(renderer), Some(settings)) => format!("{renderer}:{settings}"),
        _ => "unverified".to_owned(),
    }
}

fn thumbnail_version_current(
    library: &Library,
    context: &AssetContext,
    card: &ReadCard,
) -> CoreResult<bool> {
    let Some(report) = card.manifest.thumbnail.as_ref()
        .and_then(|thumbnail| thumbnail.render_report.as_ref()) else { return Ok(false); };
    let expected = if context.asset_type == AssetType::Motion {
        library.motion_preview_model()?.and_then(|path| {
            crate::thumbnail::motion_preview_settings_version(Path::new(&path)).ok()
        })
    } else if context.asset_type == AssetType::Scene {
        Some(crate::thumbnail::SCENE_PREVIEW_SETTINGS_VERSION.to_owned())
    } else {
        Some(crate::thumbnail::PREVIEW_SETTINGS_VERSION.to_owned())
    };
    Ok(report.renderer_version == crate::thumbnail::RENDERER_VERSION
        && expected.as_deref() == Some(report.preview_settings_version.as_str()))
}

pub(crate) fn create(
    library: &Library,
    asset_id: &str,
    preview_webp: Option<&[u8]>,
    render_report: Option<&ThumbnailRenderReport>,
) -> CoreResult<CardResult> {
    create_checked(library, asset_id, preview_webp, render_report, None)
}

pub(crate) fn create_for_asset(
    library: &Library,
    asset: &Asset,
    preview_webp: Option<&[u8]>,
    render_report: Option<&ThumbnailRenderReport>,
) -> CoreResult<CardResult> {
    create_checked(library, &asset.id, preview_webp, render_report, Some(asset))
}

fn create_checked(
    library: &Library,
    asset_id: &str,
    preview_webp: Option<&[u8]>,
    render_report: Option<&ThumbnailRenderReport>,
    expected_asset: Option<&Asset>,
) -> CoreResult<CardResult> {
    if crate::operations::is_asset_operation_active(library, asset_id)? {
        return Err(CoreError::AssetOperation("该资产有进行中或待恢复的文件操作，暂时不能创建资源卡".to_owned()));
    }
    let context = load_context(library, asset_id)?;
    if expected_asset.is_some_and(|asset| asset.fingerprint != context.fingerprint || Path::new(&asset.primary_source) != context.source_path) {
        return Err(CoreError::Card("资产在缩略图生成期间发生变化，请重新生成".to_owned()));
    }
    if let Some(report) = render_report {
        let rendered_revision = format!("{}:{}", report.renderer_version, report.preview_settings_version);
        if rendered_revision != expected_renderer_revision(library, context.asset_type)? {
            return Err(CoreError::Card("缩略图预览设置在生成期间发生变化，请重新生成".to_owned()));
        }
    }
    if context.fingerprint.is_empty() {
        return Err(CoreError::Card(
            "源文件尚无有效指纹，请先成功扫描并解析资产".to_owned(),
        ));
    }
    if let Some(preview) = preview_webp {
        validate_webp(preview)?;
    }
    let current_fingerprint = fingerprint_file(&context.source_path)?;
    if current_fingerprint != context.fingerprint {
        return Err(CoreError::Card(
            "源文件在上次扫描后发生变化，请先重新扫描".to_owned(),
        ));
    }

    let preserved_card = if preview_webp.is_none() {
        context
            .current_card_path
            .as_deref()
            .filter(|path| path.is_file())
            .and_then(|path| read_card(path).ok())
            .filter(|card| {
                card.manifest.asset_id == context.asset_id
                    && card.manifest.source.fingerprint == context.fingerprint
            })
    } else {
        None
    };
    let preserved_preview = preserved_card
        .as_ref()
        .and_then(|card| card.preview_webp.as_deref());
    let preview_webp = preview_webp.or(preserved_preview);
    let render_report = render_report.cloned().or_else(|| {
        preserved_card
            .as_ref()
            .and_then(|card| card.manifest.thumbnail.as_ref())
            .and_then(|thumbnail| thumbnail.render_report.clone())
    });
    let expected_renderer_revision = expected_renderer_revision(library, context.asset_type)?;
    let renderer_revision = if preview_webp.is_some() {
        render_report
            .as_ref()
            .map(|report| format!("{}:{}", report.renderer_version, report.preview_settings_version))
            .unwrap_or_else(|| "unverified".to_owned())
    } else {
        expected_renderer_revision.clone()
    };
    let now = Utc::now().to_rfc3339();
    let created_at = existing_created_at(&context).unwrap_or_else(|| now.clone());
    let relative_path = relative_source(&context);
    let manifest = CardManifest {
        format: CARD_FORMAT.to_owned(),
        schema_version: CARD_SCHEMA_VERSION,
        asset_id: context.asset_id.clone(),
        asset_type: context.asset_type,
        name: context.name.clone(),
        source: ManifestSource {
            primary_file: file_name(&context.source_path),
            relative_path,
            fingerprint: context.fingerprint.clone(),
        },
        metadata: context.metadata.clone(),
        tags: context.tags.clone(),
        suppressed_tags: context.suppressed_tags.clone(),
        favorite: context.is_favorite,
        thumbnail: preview_webp.map(|_| ManifestThumbnail {
            file: "preview.webp".to_owned(),
            width: 1024,
            height: 1024,
            format: "webp".to_owned(),
            quality: 50,
            render_report,
        }),
        created_at,
        updated_at: now.clone(),
        generator: ManifestGenerator {
            name: "MMDbridgeLib".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    let manifest_bytes = manifest_json.as_bytes();
    let (target, target_kind) = choose_target(&context)?;
    let (temp_path, temp_file) = create_temp_path(&target)?;
    let temp_result = write_temp_card(temp_file, &temp_path, manifest_bytes, preview_webp);
    if let Err(error) = temp_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    let _visibility_guard = match library.visibility_guard() {
        Ok(guard) => guard,
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
    };
    let mut connection = match library.connection() {
        Ok(connection) => connection,
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
    };
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }
    };
    let current_asset: Option<(bool, String, String, String)> = match transaction
        .query_row(
            "SELECT retired_format,visibility,fingerprint,primary_source FROM assets WHERE id=?1",
            [&context.asset_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
    {
        Ok(asset) => asset,
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }
    };
    let Some((retired_format, visibility, current_fingerprint, primary_source)) = current_asset else {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::AssetNotFound(context.asset_id));
    };
    if retired_format {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::UnsupportedAssetFormat("X".to_owned()));
    }
    if visibility != "normal" {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::AssetNotFound(context.asset_id));
    }
    if current_fingerprint != context.fingerprint {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::Card(
            "源文件在资源卡准备期间发生变化，请先重新扫描".to_owned(),
        ));
    }
    let operation_active = match crate::operations::is_asset_operation_active_on(&transaction, asset_id) {
        Ok(active) => active,
        Err(error) => { let _ = fs::remove_file(&temp_path); return Err(error); }
    };
    if Path::new(&primary_source) != context.source_path || operation_active {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::Card("资产路径或文件操作状态在资源卡准备期间发生变化，请稍后重试".to_owned()));
    }

    let context_is_current = match publication_context_is_current(&transaction, &context) {
        Ok(current) => current,
        Err(error) => { let _ = fs::remove_file(&temp_path); return Err(error); }
    };
    if !context_is_current {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::Card("资产信息或资源卡在准备期间发生变化，请稍后重试".to_owned()));
    }

    let replacing = matches!(target_kind, TargetKind::Replace);
    if replacing && !card_owned_by(&target, &context.asset_id) {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::Card(format!(
            "资源卡在写入期间被替换，已保留现有文件：{}",
            target.display()
        )));
    }
    if let Err(error) = publish_card(&temp_path, &target, replacing) {
        let _ = fs::remove_file(&temp_path);
        return Err(CoreError::Card(format!(
            "无法安全发布资源卡 {}：{error}",
            target.display()
        )));
    }

    let file_signature = std::fs::metadata(&target)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| {
            (
                i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                metadata_modified_ns(&metadata),
            )
        });
    transaction.execute(
        "INSERT INTO cards(asset_id,card_path,status,manifest_json,last_checked_at,file_size,modified_ns,manifest_revision,renderer_revision,has_thumbnail)
         VALUES (?1,?2,'CardValid',?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(asset_id) DO UPDATE SET card_path=excluded.card_path,status='CardValid',
           manifest_json=excluded.manifest_json,last_checked_at=excluded.last_checked_at,
           file_size=excluded.file_size,modified_ns=excluded.modified_ns,
           manifest_revision=excluded.manifest_revision,renderer_revision=excluded.renderer_revision,
           has_thumbnail=excluded.has_thumbnail",
        params![
            context.asset_id,
            target.to_string_lossy().into_owned(),
            manifest_json,
            now,
            file_signature.map(|signature| signature.0),
            file_signature.map(|signature| signature.1),
            MANIFEST_REVISION,
            renderer_revision,
            i64::from(preview_webp.is_some()),
        ],
    )?;
    transaction.commit()?;

    Ok(CardResult {
        asset_id: context.asset_id,
        status: "CardValid".to_owned(),
        card_path: target.to_string_lossy().into_owned(),
        has_thumbnail: preview_webp.is_some(),
        message: preview_webp
            .is_none()
            .then(|| "资源卡已创建；尚无 preview.webp，缩略图待生成".to_owned()),
    })
}

pub(crate) fn expected_renderer_revision(
    library: &Library,
    asset_type: AssetType,
) -> CoreResult<String> {
    let settings = match asset_type {
        AssetType::Model => crate::thumbnail::PREVIEW_SETTINGS_VERSION.to_owned(),
        AssetType::Scene => crate::thumbnail::SCENE_PREVIEW_SETTINGS_VERSION.to_owned(),
        AssetType::Motion => match library.motion_preview_model()? {
            Some(path) => crate::thumbnail::motion_preview_settings_version(Path::new(&path))
                .unwrap_or_else(|_| "unavailable".to_owned()),
            None => "unavailable".to_owned(),
        },
    };
    Ok(format!("{}:{settings}", crate::thumbnail::RENDERER_VERSION))
}

fn metadata_modified_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0_i64, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        })
}

pub(crate) fn read_thumbnail(library: &Library, asset_id: &str) -> CoreResult<Option<Vec<u8>>> {
    let context = load_context(library, asset_id)?;
    let Some(path) = context
        .current_card_path
        .as_deref()
        .filter(|path| path.is_file())
    else {
        return Ok(None);
    };
    let card = read_card(path)?;
    if card.manifest.asset_id != context.asset_id
        || card.manifest.source.fingerprint != context.fingerprint
        || card.manifest.generator.name != "MMDbridgeLib"
        || card.manifest.generator.version != env!("CARGO_PKG_VERSION")
        || !thumbnail_version_current(library, &context, &card)?
    {
        return Ok(None);
    }
    Ok(card.preview_webp)
}

pub(crate) fn cached_thumbnail(
    library: &Library,
    asset_id: &str,
    expected_preview_settings_version: &str,
) -> CoreResult<Option<(Vec<u8>, ThumbnailRenderReport)>> {
    let status: Option<String> = library
        .connection()?
        .query_row(
            "SELECT status FROM cards WHERE asset_id=?1",
            [asset_id],
            |row| row.get(0),
        )
        .optional()?;
    if status.as_deref() != Some("CardValid") {
        return Ok(None);
    }
    let context = load_context(library, asset_id)?;
    let Some(path) = context
        .current_card_path
        .as_deref()
        .filter(|path| path.is_file())
    else {
        return Ok(None);
    };
    let card = read_card(path)?;
    if card.manifest.asset_id != context.asset_id
        || card.manifest.source.fingerprint != context.fingerprint
    {
        return Ok(None);
    }
    let Some(thumbnail) = card.manifest.thumbnail.as_ref() else {
        return Ok(None);
    };
    let Some(report) = thumbnail.render_report.as_ref() else {
        return Ok(None);
    };
    if report.renderer_version != crate::thumbnail::RENDERER_VERSION
        || report.preview_settings_version != expected_preview_settings_version
    {
        return Ok(None);
    }
    let Some(preview) = card.preview_webp else {
        return Ok(None);
    };
    Ok(Some((preview, report.clone())))
}

fn load_context(library: &Library, asset_id: &str) -> CoreResult<AssetContext> {
    library.ensure_asset_visible(asset_id)?;
    let connection = library.connection()?;
    load_context_on(&connection, asset_id)
}

fn publication_context_is_current(connection: &Connection, expected: &AssetContext) -> CoreResult<bool> {
    let current = load_context_on(connection, &expected.asset_id)?;
    Ok(current.asset_type == expected.asset_type && current.name == expected.name
        && current.source_path == expected.source_path && current.root_path == expected.root_path
        && current.fingerprint == expected.fingerprint && current.metadata == expected.metadata
        && current.tags == expected.tags && current.suppressed_tags == expected.suppressed_tags
        && current.is_favorite == expected.is_favorite
        && current.current_card_path == expected.current_card_path
        && current.current_manifest_json == expected.current_manifest_json)
}

fn load_context_on(connection: &Connection, asset_id: &str) -> CoreResult<AssetContext> {
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        bool,
    )> =
        connection
            .query_row(
                "SELECT a.id,a.asset_type,a.name,a.primary_source,r.path,a.fingerprint,c.card_path,c.manifest_json,
                        EXISTS(SELECT 1 FROM favorites f WHERE f.asset_id=a.id)
                 FROM assets a JOIN roots r ON r.id=a.root_id LEFT JOIN cards c ON c.asset_id=a.id
                 WHERE a.id=?1",
                [asset_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .optional()?;
    let Some((
        id,
        asset_type,
        name,
        source,
        root,
        fingerprint,
        card_path,
        manifest_json,
        is_favorite,
    )) = row
    else {
        return Err(CoreError::AssetNotFound(asset_id.to_owned()));
    };
    let asset_type = AssetType::parse(&asset_type)
        .ok_or_else(|| CoreError::Card(format!("未知资产类型：{asset_type}")))?;
    let metadata = connection
        .query_row(
            "SELECT value_json FROM metadata WHERE asset_id=?1 AND key='parsed'",
            [asset_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let mut statement = connection.prepare(
        "SELECT t.name FROM asset_tags at JOIN tags t ON t.id=at.tag_id WHERE at.asset_id=?1 ORDER BY t.name COLLATE NOCASE",
    )?;
    let tags = statement
        .query_map([asset_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = connection.prepare(
        "SELECT normalized_name FROM asset_tag_overrides WHERE asset_id=?1 ORDER BY normalized_name",
    )?;
    let suppressed_tags = statement
        .query_map([asset_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(AssetContext {
        asset_id: id,
        asset_type,
        name,
        source_path: PathBuf::from(source),
        root_path: PathBuf::from(root),
        fingerprint,
        metadata,
        tags,
        suppressed_tags,
        is_favorite,
        current_card_path: card_path.map(PathBuf::from),
        current_manifest_json: manifest_json,
    })
}

fn read_card(path: &Path) -> CoreResult<ReadCard> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| CoreError::Card(format!("不是可读的 ZIP 容器：{error}")))?;
    if archive.len() == 0 || archive.len() > 2 {
        return Err(CoreError::Card(
            "容器只能包含 manifest.json 和可选的 preview.webp".to_owned(),
        ));
    }
    let mut names = HashSet::new();
    for name in archive.file_names() {
        if !names.insert(name.to_owned()) || (name != "manifest.json" && name != "preview.webp") {
            return Err(CoreError::Card(format!("不支持或重复的容器条目：{name}")));
        }
    }
    if !names.contains("manifest.json") {
        return Err(CoreError::Card("容器缺少 manifest.json".to_owned()));
    }
    let manifest_bytes = read_entry(&mut archive, "manifest.json", MAX_MANIFEST_BYTES)?;
    let manifest: CardManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    let has_preview_entry = names.contains("preview.webp");
    let preview_webp = match &manifest.thumbnail {
        Some(thumbnail) => {
            if thumbnail.file != "preview.webp"
                || thumbnail.width != 1024
                || thumbnail.height != 1024
                || !thumbnail.format.eq_ignore_ascii_case("webp")
                || thumbnail.quality != 50
            {
                return Err(CoreError::Card(
                    "缩略图规格必须为 1024×1024 WebP、quality 50".to_owned(),
                ));
            }
            if !has_preview_entry {
                return Err(CoreError::Card(
                    "manifest 声明了 preview.webp，但容器内不存在该文件".to_owned(),
                ));
            }
            let preview = read_entry(&mut archive, "preview.webp", MAX_PREVIEW_BYTES)?;
            validate_webp(&preview)?;
            Some(preview)
        }
        None if has_preview_entry => {
            return Err(CoreError::Card(
                "容器包含未在 manifest 中声明的 preview.webp".to_owned(),
            ));
        }
        None => None,
    };
    let manifest_json = String::from_utf8(manifest_bytes)
        .map_err(|error| CoreError::Card(format!("manifest.json 不是 UTF-8：{error}")))?;
    Ok(ReadCard {
        manifest,
        manifest_json,
        preview_webp,
    })
}

fn validate_manifest(manifest: &CardManifest) -> CoreResult<()> {
    if manifest.format != CARD_FORMAT {
        return Err(CoreError::Card(format!(
            "未知资源卡格式：{}",
            manifest.format
        )));
    }
    if manifest.schema_version != CARD_SCHEMA_VERSION {
        return Err(CoreError::Card(format!(
            "不支持的 MMDRCV schema_version：{}",
            manifest.schema_version
        )));
    }
    if manifest.asset_id.trim().is_empty() || manifest.name.trim().is_empty() {
        return Err(CoreError::Card("asset_id 与 name 不能为空".to_owned()));
    }
    if manifest.asset_id.len() > 128
        || manifest.name.len() > 4096
        || manifest.generator.name.trim().is_empty()
        || manifest.generator.version.trim().is_empty()
        || DateTime::parse_from_rfc3339(&manifest.created_at).is_err()
        || DateTime::parse_from_rfc3339(&manifest.updated_at).is_err()
    {
        return Err(CoreError::Card(
            "manifest 必填字段或时间格式无效".to_owned(),
        ));
    }
    if manifest.source.primary_file.trim().is_empty()
        || manifest.source.primary_file.contains(['/', '\\'])
        || manifest.source.relative_path.trim().is_empty()
        || Path::new(&manifest.source.relative_path).is_absolute()
        || manifest
            .source
            .relative_path
            .split(['/', '\\'])
            .any(|part| part == "..")
    {
        return Err(CoreError::Card(
            "source 路径必须是安全的相对路径".to_owned(),
        ));
    }
    let Some(hash) = manifest.source.fingerprint.strip_prefix("blake3:") else {
        return Err(CoreError::Card(
            "source.fingerprint 必须使用 blake3: 前缀".to_owned(),
        ));
    };
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::Card(
            "source.fingerprint 的 BLAKE3 值格式无效".to_owned(),
        ));
    }
    for tag in manifest.tags.iter().chain(&manifest.suppressed_tags) {
        if tag.trim() != tag
            || tag.len() > 128
            || tag.is_empty()
            || tag.chars().any(char::is_control)
        {
            return Err(CoreError::Card("manifest.tags 包含无效标签".to_owned()));
        }
    }
    Ok(())
}

fn read_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    max_bytes: u64,
) -> CoreResult<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .map_err(|error| CoreError::Card(format!("无法读取 {name}：{error}")))?;
    if entry.size() > max_bytes {
        return Err(CoreError::Card(format!("{name} 超过大小上限")));
    }
    let mut contents = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut contents)
        .map_err(|error| CoreError::Card(format!("读取 {name} 失败或 CRC 校验失败：{error}")))?;
    Ok(contents)
}

fn choose_target(context: &AssetContext) -> CoreResult<(PathBuf, TargetKind)> {
    let default = default_card_path(&context.source_path, &context.name);
    if let Some(preferred) = &context.current_card_path {
        if preferred.is_file() && card_owned_by(preferred, &context.asset_id) {
            return Ok((preferred.clone(), TargetKind::Replace));
        }
    }
    if !default.exists() {
        return Ok((default, TargetKind::New));
    }
    if card_owned_by(&default, &context.asset_id) {
        return Ok((default, TargetKind::Replace));
    }

    let parent = default.parent().unwrap_or(Path::new("."));
    let stem = default
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Asset");
    let short = &context.asset_id[..context.asset_id.len().min(8)];
    let suffixed = parent.join(format!("{stem}__{short}.MMDRCV"));
    if !suffixed.exists() {
        return Ok((suffixed, TargetKind::New));
    }
    if card_owned_by(&suffixed, &context.asset_id) {
        return Ok((suffixed, TargetKind::Replace));
    }
    let full = parent.join(format!("{stem}__{}.MMDRCV", context.asset_id));
    if !full.exists() {
        return Ok((full, TargetKind::New));
    }
    if card_owned_by(&full, &context.asset_id) {
        return Ok((full, TargetKind::Replace));
    }
    Err(CoreError::Card(format!(
        "资源卡名称冲突，且无法安全覆盖：{}",
        default.display()
    )))
}

fn card_owned_by(path: &Path, asset_id: &str) -> bool {
    read_card(path).is_ok_and(|card| card.manifest.asset_id == asset_id)
}

fn find_owned_neighbor(source_path: &Path, asset_id: &str) -> Option<PathBuf> {
    let parent = source_path.parent()?;
    let mut matches = fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("MMDRCV"))
        })
        .filter(|path| card_owned_by(path, asset_id))
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}

fn create_temp_path(target: &Path) -> CoreResult<(PathBuf, File)> {
    let parent = target.parent().unwrap_or(Path::new("."));
    for _ in 0..5 {
        let temp = parent.join(format!(".MMDRCV-{}.tmp", Uuid::new_v4()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(CoreError::Card("无法创建资源卡临时文件".to_owned()))
}

fn write_temp_card(
    file: File,
    path: &Path,
    manifest: &[u8],
    preview: Option<&[u8]>,
) -> CoreResult<()> {
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer
        .start_file("manifest.json", options)
        .map_err(|error| CoreError::Card(format!("创建 manifest.json 失败：{error}")))?;
    writer.write_all(manifest)?;
    if let Some(preview) = preview {
        writer
            .start_file("preview.webp", options)
            .map_err(|error| CoreError::Card(format!("创建 preview.webp 失败：{error}")))?;
        writer.write_all(preview)?;
    }
    let file = writer
        .finish()
        .map_err(|error| CoreError::Card(format!("完成 MMDRCV ZIP 失败：{error}")))?;
    file.sync_all()?;
    drop(file);
    read_card(path)?;
    Ok(())
}

fn publish_card(temp: &Path, target: &Path, replace: bool) -> std::io::Result<()> {
    if replace {
        replace_file(temp, target)
    } else {
        move_new_file(temp, target)
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    windows_move_file(source, target, true)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn move_new_file(source: &Path, target: &Path) -> std::io::Result<()> {
    windows_move_file(source, target, false)
}

#[cfg(not(windows))]
fn move_new_file(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::hard_link(source, target)?;
    fs::remove_file(source)
}

#[cfg(windows)]
fn windows_move_file(source: &Path, target: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing_name: *const u16, new_name: *const u16, flags: u32) -> i32;
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    let result = unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), flags) };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn validate_webp(bytes: &[u8]) -> CoreResult<()> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(CoreError::Card("缩略图不是有效的 WebP 容器".to_owned()));
    }
    let declared_size = u32::from_le_bytes(bytes[4..8].try_into().expect("fixed-size slice"));
    if declared_size < 4 || declared_size as usize + 8 > bytes.len() {
        return Err(CoreError::Card("WebP 容器长度无效".to_owned()));
    }
    if bytes.len() as u64 > MAX_PREVIEW_BYTES {
        return Err(CoreError::Card("缩略图超过 32 MiB 上限".to_owned()));
    }
    Ok(())
}

fn fingerprint_file(path: &Path) -> CoreResult<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn existing_created_at(context: &AssetContext) -> Option<String> {
    let json = context.current_manifest_json.as_deref()?;
    let manifest = serde_json::from_str::<CardManifest>(json).ok()?;
    (manifest.asset_id == context.asset_id).then_some(manifest.created_at)
}

fn relative_source(context: &AssetContext) -> String {
    context
        .source_path
        .strip_prefix(&context.root_path)
        .unwrap_or(&context.source_path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(windows)]
fn path_text_equal(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

#[cfg(not(windows))]
fn path_text_equal(left: &str, right: &str) -> bool {
    left == right
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn default_card_path(source_path: &Path, display_name: &str) -> PathBuf {
    let safe = safe_filename(display_name);
    source_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{safe}.MMDRCV"))
}

fn safe_filename(name: &str) -> String {
    let mut safe = name
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    safe = safe.trim_end_matches([' ', '.']).to_owned();
    while safe.encode_utf16().count() > 120 {
        safe.pop();
    }
    if safe.is_empty()
        || matches!(
            safe.to_ascii_uppercase().as_str(),
            "CON" | "PRN" | "AUX" | "NUL"
        )
        || is_windows_numbered_device(&safe)
    {
        safe.insert(0, '_');
    }
    if safe.is_empty() {
        "Asset".to_owned()
    } else {
        safe
    }
}

fn is_windows_numbered_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication_library() -> Library {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at) VALUES ('r','model','/r','/r','Root','now');
             INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,created_at,updated_at,last_seen_at)
             VALUES ('a','r','model','A','/r/a/model.pmx','/r/a','before','now','now','now');
             INSERT INTO metadata(asset_id,key,value_json) VALUES ('a','parsed','{}');
             INSERT INTO cards(asset_id,card_path,status,manifest_json,last_checked_at) VALUES ('a','/r/a/A.MMDRCV','CardValid','{}','now');"
        ).unwrap();
        library
    }

    #[test]
    fn publication_rejects_annotations_changed_after_preparation() {
        let library = publication_library();
        let original = load_context(&library, "a").unwrap();
        assert!(publication_context_is_current(&library.connection().unwrap(), &original).unwrap());
        library.set_favorite("a", true).unwrap();
        assert!(!publication_context_is_current(&library.connection().unwrap(), &original).unwrap());
        let favorite = load_context(&library, "a").unwrap();
        assert!(publication_context_is_current(&library.connection().unwrap(), &favorite).unwrap());
        library.add_asset_tag("a", "新标签", "user", None).unwrap();
        assert!(!publication_context_is_current(&library.connection().unwrap(), &favorite).unwrap());
        let tagged = load_context(&library, "a").unwrap();
        library.remove_asset_tag("a", "新标签").unwrap();
        assert!(!publication_context_is_current(&library.connection().unwrap(), &tagged).unwrap());
        let removed = load_context(&library, "a").unwrap();
        library.connection().unwrap().execute(r#"UPDATE metadata SET value_json='{"bone_count":20}' WHERE asset_id='a' AND key='parsed'"#, []).unwrap();
        assert!(!publication_context_is_current(&library.connection().unwrap(), &removed).unwrap());
    }

    #[test]
    fn publication_rejects_a_card_updated_by_another_writer() {
        let library = publication_library();
        let prepared = load_context(&library, "a").unwrap();
        library.connection().unwrap().execute(r#"UPDATE cards SET manifest_json='{"updated_at":"later"}' WHERE asset_id='a'"#, []).unwrap();
        assert!(!publication_context_is_current(&library.connection().unwrap(), &prepared).unwrap());
        let current = load_context(&library, "a").unwrap();
        assert!(publication_context_is_current(&library.connection().unwrap(), &current).unwrap());
    }

    #[test]
    fn rendered_asset_version_cannot_be_published_as_a_newly_scanned_version() {
        let library = Library::in_memory().unwrap();
        library.connection().unwrap().execute_batch(
            "INSERT INTO roots(id,asset_type,path,path_key,display_name,created_at) VALUES ('r','model','/r','/r','Root','now');
             INSERT INTO assets(id,root_id,asset_type,name,primary_source,asset_directory,fingerprint,created_at,updated_at,last_seen_at)
             VALUES ('a','r','model','A','/r/a/model.pmx','/r/a','before','now','now','now');"
        ).unwrap();
        let rendered = library.inspect_asset("a").unwrap();
        library.connection().unwrap().execute("UPDATE assets SET fingerprint='after' WHERE id='a'", []).unwrap();
        assert!(matches!(create_for_asset(&library, &rendered, None, None), Err(CoreError::Card(message)) if message.contains("缩略图生成期间")));
    }

    #[test]
    fn card_metadata_small_dimensions_survive_json_roundtrip() {
        let metadata: serde_json::Value = serde_json::from_str(
            r#"{"width":1.9485949565023478e-7,"area":6.3703458713709e-7}"#,
        )
        .unwrap();
        let restored: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&metadata).unwrap()).unwrap();
        assert_eq!(metadata, restored);
    }
}
