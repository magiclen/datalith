use std::{io, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("content writes are paused for an export")]
    Busy,
    #[error("task cancelled")]
    Cancelled,
    #[error("upload exceeds the configured limit")]
    PayloadTooLarge,
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Internal(String),
}

impl ServiceError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::Busy => "writes_paused",
            Self::Cancelled => "cancelled",
            Self::PayloadTooLarge => "payload_too_large",
            Self::Unsupported(_) => "unsupported_media",
            Self::Io(_) | Self::Database(_) | Self::Json(_) | Self::Internal(_) => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    #[default]
    Resource,
    Image,
    Audio,
    Video,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    pub expires_in_seconds: Option<u64>,
    pub single_use:         bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropRatio {
    pub width:  f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageVariantSpec {
    pub name:        String,
    pub max_width:   Option<u32>,
    pub max_height:  Option<u32>,
    pub crop:        Option<CropRatio>,
    pub multipliers: Vec<u8>,
}

impl Default for ImageVariantSpec {
    fn default() -> Self {
        Self {
            name:        "default".into(),
            max_width:   None,
            max_height:  None,
            crop:        None,
            multipliers: vec![1, 2, 3],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageOptions {
    pub variants:      Vec<ImageVariantSpec>,
    pub save_original: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            variants: vec![ImageVariantSpec::default()], save_original: true
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageLimits {
    pub max_pixels:       u64,
    pub max_frames:       u32,
    pub max_total_pixels: u64,
    pub max_variants:     usize,
    pub max_multiplier:   u8,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_pixels:       50_000_000,
            max_frames:       500,
            max_total_pixels: 100_000_000,
            max_variants:     16,
            max_multiplier:   3,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UploadOptions {
    pub kind:      MediaKind,
    pub file_name: Option<String>,
    pub file_type: Option<String>,
    pub retention: Retention,
    pub image:     ImageOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFile {
    pub id:        Uuid,
    pub sha256:    String,
    pub file_size: String,
    pub file_type: String,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub name:         String,
    pub multiplier:   u8,
    pub format:       String,
    pub width:        u32,
    pub height:       u32,
    pub animated:     bool,
    pub file:         MediaFile,
    pub content_path: String,
    pub recipe:       Option<ImageVariantSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id:          Uuid,
    pub kind:        MediaKind,
    pub created_at:  DateTime<Utc>,
    pub file_name:   String,
    pub original:    Option<MediaFile>,
    pub variants:    Vec<Variant>,
    pub expires_at:  Option<DateTime<Utc>>,
    pub single_use:  bool,
    pub consumed_at: Option<DateTime<Utc>>,
    pub animated:    bool,
    pub frame_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskError {
    pub code:    String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id:              Uuid,
    pub kind:            String,
    pub status:          TaskStatus,
    pub stage:           String,
    pub completed_units: u64,
    pub total_units:     Option<u64>,
    pub attempt:         u32,
    pub created_at:      DateTime<Utc>,
    pub updated_at:      DateTime<Utc>,
    pub result:          Option<serde_json::Value>,
    pub error:           Option<TaskError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items:    Vec<T>,
    pub page:     u64,
    pub per_page: u64,
    pub total:    String,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub max_file_size:          u64,
    pub workers:                usize,
    pub task_retention_seconds: u64,
    pub image_limits:           ImageLimits,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_file_size:          2 * 1024 * 1024 * 1024,
            workers:                1,
            task_retention_seconds: 7 * 24 * 60 * 60,
            image_limits:           ImageLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentRequest {
    pub variant:    Option<String>,
    pub multiplier: Option<u8>,
    pub format:     Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportOptions {
    pub ids: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessOptions {
    pub image: ImageOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Work {
    Upload {
        options: UploadOptions,
        hash:    String,
    },
    Process {
        source:     Uuid,
        options:    ProcessOptions,
        hash:       String,
        file_name:  String,
        expires_at: Option<DateTime<Utc>>,
    },
    Import {
        hash: String,
    },
    Export(ExportOptions),
}

#[derive(Debug)]
pub(super) struct PreparedFile {
    pub path:     PathBuf,
    pub metadata: MediaFile,
}
