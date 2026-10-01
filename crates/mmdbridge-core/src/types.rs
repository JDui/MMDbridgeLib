use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssetType {
    Model,
    Motion,
    Scene,
}

impl AssetType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Motion => "motion",
            Self::Scene => "scene",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "model" => Some(Self::Model),
            "motion" => Some(Self::Motion),
            "scene" | "stage" => Some(Self::Scene),
            _ => None,
        }
    }

    pub(crate) fn accepts_extension(self, extension: &str) -> bool {
        match self {
            Self::Model => extension.eq_ignore_ascii_case("pmx"),
            Self::Motion => {
                extension.eq_ignore_ascii_case("vmd") || extension.eq_ignore_ascii_case("vpd")
            }
            Self::Scene => {
                extension.eq_ignore_ascii_case("pmx")
                    || extension.eq_ignore_ascii_case("pmd")
            }
        }
    }

    pub fn supports_thumbnail_extension(self, extension: &str) -> bool {
        match self {
            Self::Model => extension.eq_ignore_ascii_case("pmx"),
            Self::Motion => {
                extension.eq_ignore_ascii_case("vmd") || extension.eq_ignore_ascii_case("vpd")
            }
            Self::Scene => {
                extension.eq_ignore_ascii_case("pmx")
                    || extension.eq_ignore_ascii_case("pmd")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub id: String,
    pub asset_type: AssetType,
    pub path: String,
    pub display_name: String,
    pub enabled: bool,
    pub scan_recursive: bool,
    pub created_at: String,
    pub last_scan_at: Option<String>,
    pub scan_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub asset_type: AssetType,
    pub root_id: String,
    pub name: String,
    pub primary_source: String,
    pub asset_directory: String,
    pub fingerprint: String,
    pub metadata: Value,
    pub statuses: Vec<String>,
    pub card_status: String,
    pub has_thumbnail: bool,
    pub is_favorite: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetCursor {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetPage {
    pub items: Vec<AssetListItem>,
    pub next_cursor: Option<AssetCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDirectory {
    pub path: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    pub path: String,
    pub visible_count: i64,
    pub child_directories: Vec<AssetDirectory>,
    pub adjusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetListItem {
    pub id: String,
    pub asset_type: AssetType,
    pub root_id: String,
    pub name: String,
    pub primary_source: String,
    pub asset_directory: String,
    pub metadata: Value,
    pub statuses: Vec<String>,
    pub card_status: String,
    pub has_thumbnail: bool,
    pub is_favorite: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetTag {
    pub name: String,
    pub source: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagMutation {
    pub asset_id: String,
    pub name: String,
    pub source: String,
    pub changed: bool,
    pub blocked_by_user: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetRelation {
    pub id: String,
    pub relation_type: String,
    pub source_asset: String,
    pub target_asset: String,
    pub confidence: f64,
    pub reason: Value,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationRefreshReport {
    pub motion_camera_pairs: usize,
    pub version_families: usize,
    pub relation_proposals: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum FilterExpr {
    And {
        children: Vec<FilterExpr>,
    },
    Or {
        children: Vec<FilterExpr>,
    },
    Not {
        child: Box<FilterExpr>,
    },
    Rule {
        field: FilterField,
        operator: FilterOperator,
        value: Value,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterField {
    AssetType,
    RootId,
    Directory,
    Tag,
    Favorite,
    CardStatus,
    // Keep this variant so persisted filters from older schemas can be loaded and disabled safely.
    DuplicateStatus,
    RelationStatus,
    RecentlyAdded,
    RecentlyModified,
    NeedsReview,
    PolygonCount,
    BoneCount,
    SkeletonClass,
    HasThumbnail,
    HasCard,
    FrameCount,
    Duration,
    HasBoneMotion,
    HasMorphMotion,
    HasCamera,
    CameraOnly,
    Pose,
    HasPairedCamera,
    FileType,
    Width,
    Depth,
    Area,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOperator {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedFilter {
    pub id: String,
    pub name: String,
    pub expression: FilterExpr,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardValidation {
    pub asset_id: String,
    pub status: String,
    pub card_path: Option<String>,
    pub has_thumbnail: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardResult {
    pub asset_id: String,
    pub status: String,
    pub card_path: String,
    pub has_thumbnail: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub root_id: String,
    pub files_seen: usize,
    pub assets_added: usize,
    pub assets_updated: usize,
    pub assets_unchanged: usize,
    pub parse_failures: usize,
    pub unsupported_files: usize,
    pub missing_sources: usize,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanState {
    pub root_id: String,
    pub status: String,
    pub queue_order: i64,
    pub full_check: bool,
    pub scope: String,
    pub progress: f64,
    pub files_seen: usize,
    pub files_processed: usize,
    pub error: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanChangeKind {
    File,
    Subtree,
    Removed,
    Root,
}

#[derive(Debug, Clone)]
pub struct ScanChange {
    pub path: String,
    pub kind: ScanChangeKind,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingScanChange {
    pub path: String,
    pub scope: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ScanWork {
    pub generation: i64,
    pub full_check: bool,
    pub changes: Vec<PendingScanChange>,
}

#[derive(Debug)]
pub(crate) struct ParsedCandidate {
    pub name: String,
    pub metadata: Value,
    pub status: String,
    pub dependencies: Vec<ParsedDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ParsedDependency {
    pub reference: String,
    pub role: String,
    pub path: Option<String>,
    pub status: String,
}

pub(crate) fn display_name(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unnamed Asset".to_owned())
}
