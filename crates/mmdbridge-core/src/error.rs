use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("数据库版本 {found} 高于当前程序支持的版本 {supported}，请使用新版程序打开")]
    UnsupportedDatabaseVersion { found: i64, supported: i64 },
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid root path: {0}")]
    InvalidRoot(String),
    #[error("asset root was not found: {0}")]
    RootNotFound(String),
    #[error("asset root scanning is paused: {0}")]
    RootDisabled(String),
    #[error("asset root scan was cancelled")]
    ScanCancelled,
    #[error("asset root scan was paused")]
    ScanPaused,
    #[error("storage limit: {0}")]
    StorageLimit(String),
    #[error("model preview error: {0}")]
    ModelPreview(String),
    #[error("thumbnail render error: {0}")]
    ThumbnailRender(String),
    #[error("thumbnail queue error: {0}")]
    ThumbnailQueue(String),
    #[error("asset operation error: {0}")]
    AssetOperation(String),
    #[error("thumbnail job was cancelled")]
    ThumbnailCancelled,
    #[error("job was not found: {0}")]
    JobNotFound(String),
    #[error("asset was not found: {0}")]
    AssetNotFound(String),
    #[error("格式已不再支持：{0}")]
    UnsupportedAssetFormat(String),
    #[error("invalid asset type: {0}")]
    InvalidAssetType(String),
    #[error("resource card error: {0}")]
    Card(String),
    #[error("invalid tag: {0}")]
    InvalidTag(String),
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
    #[error("metadata serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("library lock is poisoned")]
    LockPoisoned,
}

impl CoreError {
    /// Whether this request can be retried immediately with the same arguments.
    ///
    /// Known deterministic request/state errors, unsupported/existing-target file
    /// operations, and explicit cancellations return false. Other error categories
    /// keep the existing retryable default until they carry more specific context.
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::Io(error) => !matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::AlreadyExists
            ),
            Self::Json(_) => false,
            _ => !matches!(
                self,
                Self::RootNotFound(_)
                    | Self::UnsupportedDatabaseVersion { .. }
                    | Self::RootDisabled(_)
                    | Self::ScanCancelled
                    | Self::ScanPaused
                    | Self::StorageLimit(_)
                    | Self::ThumbnailCancelled
                    | Self::JobNotFound(_)
                    | Self::AssetNotFound(_)
                    | Self::UnsupportedAssetFormat(_)
                    | Self::InvalidAssetType(_)
                    | Self::InvalidTag(_)
                    | Self::InvalidFilter(_)
                    | Self::LockPoisoned
            ),
        }
    }
}
