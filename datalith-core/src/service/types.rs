use std::{io, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Errors from `DatalithService`.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The request or its data is invalid.
    #[error("{0}")]
    Invalid(String),
    /// The item does not exist or is no longer available.
    #[error("not found")]
    NotFound,
    /// The request conflicts with the current state.
    #[error("{0}")]
    Conflict(String),
    /// Content writes are paused while an export runs.
    #[error("content writes are paused for an export")]
    Busy,
    /// The task was cancelled.
    #[error("task cancelled")]
    Cancelled,
    /// The upload is larger than the configured limit.
    #[error("upload exceeds the configured limit")]
    PayloadTooLarge,
    /// The media type or the feature is not supported.
    #[error("{0}")]
    Unsupported(String),
    /// A file system error.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A database error.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// A JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// An unexpected internal error.
    #[error("{0}")]
    Internal(String),
}

impl ServiceError {
    /// Get the error code used in API responses.
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

/// The kind of a media item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A file that is stored as it is.
    #[default]
    Resource,
    /// An image with generated variants.
    Image,
    /// Reserved for audio; not available yet.
    Audio,
    /// Reserved for video; not available yet.
    Video,
}

/// How long uploaded media is kept.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    /// Remove the media after this many seconds, from 1 to 36,000,000.
    pub expires_in_seconds: Option<u64>,
    /// Allow the content to be downloaded only once.
    pub single_use:         bool,
}

/// The width-to-height ratio of a center crop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CropRatio {
    /// The width part of the ratio.
    pub width:  f64,
    /// The height part of the ratio.
    pub height: f64,
}

/// A recipe for one named set of image outputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageVariantSpec {
    /// The variant name: 1 to 64 ASCII letters, digits, `_`, or `-`, and not `original`.
    pub name:        String,
    /// The maximum width of the 1x output in pixels.
    pub max_width:   Option<u32>,
    /// The maximum height of the 1x output in pixels.
    pub max_height:  Option<u32>,
    /// Crop the center of the image to this ratio first.
    pub crop:        Option<CropRatio>,
    /// The output scales; they must be unique and include 1.
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

/// Options for image processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageOptions {
    /// The variants to create.
    pub variants:      Vec<ImageVariantSpec>,
    /// Keep the uploaded file as the original.
    pub save_original: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            variants: vec![ImageVariantSpec::default()], save_original: true
        }
    }
}

/// Limits that protect the service from very large images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageLimits {
    /// The maximum pixel count of one frame.
    pub max_pixels:       u64,
    /// The maximum number of frames.
    pub max_frames:       u32,
    /// The maximum pixel count of all frames together.
    pub max_total_pixels: u64,
    /// The maximum number of variants in one request.
    pub max_variants:     usize,
    /// The largest allowed output scale.
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

/// Options for an upload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UploadOptions {
    /// The kind of media to create.
    pub kind:      MediaKind,
    /// The name of the media; the task ID is used when it is missing.
    pub file_name: Option<String>,
    /// The MIME type of a resource; it is detected when it is missing.
    pub file_type: Option<String>,
    /// How long the media is kept.
    pub retention: Retention,
    /// Image options, used when `kind` is `Image`.
    pub image:     ImageOptions,
}

/// A stored file of a media item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFile {
    /// The file ID.
    pub id:        Uuid,
    /// The SHA-256 hash in lowercase hex.
    pub sha256:    String,
    /// The size in bytes, as a decimal string.
    pub file_size: String,
    /// The MIME type.
    pub file_type: String,
    /// The file name used for downloads.
    pub file_name: String,
}

/// One generated image file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    /// The name of the recipe that created this file.
    pub name:         String,
    /// The output scale.
    pub multiplier:   u8,
    /// The output format, such as `webp`, `png`, `jpeg`, or `gif`.
    pub format:       String,
    /// The width in pixels.
    pub width:        u32,
    /// The height in pixels.
    pub height:       u32,
    /// Whether the file has more than one frame.
    pub animated:     bool,
    /// The stored file.
    pub file:         MediaFile,
    /// The API path that downloads this file.
    pub content_path: String,
    /// The recipe that created this file; migrated images have none.
    pub recipe:       Option<ImageVariantSpec>,
}

/// A stored media item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    /// The media ID.
    pub id:          Uuid,
    /// The kind of media.
    pub kind:        MediaKind,
    /// The time when the media was stored.
    pub created_at:  DateTime<Utc>,
    /// The name of the media.
    pub file_name:   String,
    /// The original file, if it was kept.
    pub original:    Option<MediaFile>,
    /// The generated image files.
    pub variants:    Vec<Variant>,
    /// The time when the media expires.
    pub expires_at:  Option<DateTime<Utc>>,
    /// Whether the content can be downloaded only once.
    pub single_use:  bool,
    /// The time when the single-use content was downloaded.
    pub consumed_at: Option<DateTime<Utc>>,
    /// Whether the source image is animated.
    pub animated:    bool,
    /// The number of frames in the source image.
    pub frame_count: u32,
}

/// The state of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// The task waits for a worker.
    Queued,
    /// A worker is running the task.
    Running,
    /// The task stops at its next safe point.
    Cancelling,
    /// The task finished successfully.
    Succeeded,
    /// The task failed and can be retried.
    Failed,
    /// The task was cancelled and can be retried.
    Cancelled,
}

impl TaskStatus {
    /// Check whether the task has finished, successfully or not.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// The reason why a task did not succeed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskError {
    /// The error code, such as `invalid_request`.
    pub code:    String,
    /// A message for people.
    pub message: String,
}

/// A background job, such as an upload, an import, or an export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// The task ID.
    pub id:              Uuid,
    /// The job kind: `resource`, `image`, `import`, or `export`.
    pub kind:            String,
    /// The state of the task.
    pub status:          TaskStatus,
    /// A short word for the current step.
    pub stage:           String,
    /// The number of finished work units.
    pub completed_units: u64,
    /// The total number of work units, when it is known.
    pub total_units:     Option<u64>,
    /// How many times a worker has started this task.
    pub attempt:         u32,
    /// The time when the task was created.
    pub created_at:      DateTime<Utc>,
    /// The time when the task last changed.
    pub updated_at:      DateTime<Utc>,
    /// The result of a successful task.
    pub result:          Option<serde_json::Value>,
    /// The error of a failed or cancelled task.
    pub error:           Option<TaskError>,
}

/// One page of a list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    /// The items on this page.
    pub items:    Vec<T>,
    /// The page number, starting from 1.
    pub page:     u64,
    /// The number of items per page.
    pub per_page: u64,
    /// The total number of items, as a decimal string.
    pub total:    String,
}

/// Settings for `DatalithService`.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// The largest upload or import archive in bytes.
    pub max_file_size:          u64,
    /// The number of tasks that can run at the same time, from 1 to 64.
    pub workers:                usize,
    /// How long finished tasks are kept, in seconds.
    pub task_retention_seconds: u64,
    /// Limits for image processing.
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

/// Choose which file of a media item to open.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentRequest {
    /// The variant name, or `original`; the first variant is used when it is missing.
    pub variant:    Option<String>,
    /// The output scale; the default is 1.
    pub multiplier: Option<u8>,
    /// The output format; the default is `webp`.
    pub format:     Option<String>,
}

/// Options for an export.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportOptions {
    /// The media to export; all media are exported when it is missing.
    pub ids: Option<Vec<Uuid>>,
}

/// Options for creating new image variants from existing media.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessOptions {
    /// The image options.
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
