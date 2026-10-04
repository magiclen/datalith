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
    /// Audio with generated outputs.
    Audio,
    /// Video with HLS outputs.
    Video,
}

/// Whether compliant uploaded content can be reused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode {
    /// Encode outputs with the service recipe.
    #[default]
    Transcode,
    /// Reuse content when its metadata and packets meet the output requirements.
    Trust,
}

impl ProcessingMode {
    fn is_default(&self) -> bool {
        *self == Self::Transcode
    }
}

/// How an output was created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMethod {
    /// Older stored metadata does not record how the output was made.
    #[default]
    Unknown,
    /// The uploaded file is reused without changes.
    Copied,
    /// Encoded streams are reused in a new container.
    Remuxed,
    /// The output was encoded from the source.
    Transcoded,
}

impl ProcessingMethod {
    fn is_default(&self) -> bool {
        *self == Self::Unknown
    }
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
    /// Reuse compliant content when set to `Trust`.
    #[serde(skip_serializing_if = "ProcessingMode::is_default")]
    pub processing_mode: ProcessingMode,
    /// The variants to create.
    pub variants:        Vec<ImageVariantSpec>,
    /// Keep the uploaded file as the original.
    pub save_original:   bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            processing_mode: ProcessingMode::default(),
            variants:        vec![ImageVariantSpec::default()],
            save_original:   true,
        }
    }
}

/// Options for standalone audio processing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioOptions {
    /// Reuse compliant streams when set to `Trust`.
    pub processing_mode:   ProcessingMode,
    /// Keep the uploaded file as the original.
    pub save_original:     bool,
    /// Preserve lossless source samples in FLAC when possible, with AAC fallback.
    pub preserve_lossless: bool,
    /// The source stream index; otherwise use the default audio stream or the first one.
    pub audio_stream:      Option<u32>,
}

impl AudioOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// One requested video resolution and frame-rate pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoVariantSpec {
    /// The horizontal canvas tier, such as 720 or 1080.
    pub resolution: u16,
    /// The frame-rate tier, such as 30 or 60.
    pub fps:        u8,
}

/// Options for video processing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoOptions {
    /// Reuse compliant streams when set to `Trust`.
    pub processing_mode:   ProcessingMode,
    /// Keep the uploaded file as the original.
    pub save_original:     bool,
    /// The requested resolution and frame-rate pairs; there is no default ladder.
    pub variants:          Vec<VideoVariantSpec>,
    /// Preserve lossless source audio in FLAC when possible, with AAC fallback.
    pub preserve_lossless: bool,
    /// The source audio stream index; otherwise use the default audio stream or the first one.
    pub audio_stream:      Option<u32>,
}

impl VideoOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
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
    /// Audio options, used when `kind` is `Audio`.
    #[serde(skip_serializing_if = "AudioOptions::is_default")]
    pub audio:     AudioOptions,
    /// Video options, used when `kind` is `Video`.
    #[serde(skip_serializing_if = "VideoOptions::is_default")]
    pub video:     VideoOptions,
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
    /// How the file was created; older metadata may not record this.
    #[serde(default, skip_serializing_if = "ProcessingMethod::is_default")]
    pub processing_method: ProcessingMethod,
    /// The name of the recipe that created this file.
    pub name:              String,
    /// The output scale.
    pub multiplier:        u8,
    /// The output format, such as `webp`, `png`, `jpeg`, or `gif`.
    pub format:            String,
    /// The width in pixels.
    pub width:             u32,
    /// The height in pixels.
    pub height:            u32,
    /// Whether the file has more than one frame.
    pub animated:          bool,
    /// The stored file.
    pub file:              MediaFile,
    /// The API path that downloads this file.
    pub content_path:      String,
    /// The recipe that created this file; migrated images have none.
    pub recipe:            Option<ImageVariantSpec>,
}

/// An exact frame rate or time ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    /// The numerator.
    pub numerator:   u32,
    /// The nonzero denominator.
    pub denominator: u32,
}

/// An audio output, either a standalone file or an HLS track.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioVariant {
    /// The output identifier, also used as the HLS track identifier.
    pub id:                String,
    /// The codec, such as `aac` or `flac`.
    pub codec:             String,
    /// The average encoded bitrate in bits per second.
    pub bitrate:           u64,
    /// The sample rate in Hz.
    pub sample_rate:       u32,
    /// The channel count.
    pub channels:          u16,
    /// The number of significant bits per sample for lossless outputs.
    pub bits_per_sample:   Option<u8>,
    /// How the encoded samples were created.
    pub processing_method: ProcessingMethod,
    /// A file for standalone audio; HLS outputs have none.
    pub file:              Option<MediaFile>,
    /// The content path for standalone audio, or the HLS track playlist path.
    pub content_path:      String,
}

/// Standalone audio metadata without HLS segment details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioMedia {
    /// The presentation duration in seconds.
    pub duration_seconds: f64,
    /// The available AAC and FLAC outputs.
    pub variants:         Vec<AudioVariant>,
}

/// One generated video stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoVariant {
    /// The output identifier, also used as the HLS track identifier.
    pub id:                String,
    /// The effective resolution tier.
    pub resolution:        u16,
    /// The horizontal canvas width in pixels.
    pub width:             u32,
    /// The horizontal canvas height in pixels.
    pub height:            u32,
    /// The effective frame-rate tier.
    pub fps:               u8,
    /// The exact frame rate used by the output.
    pub frame_rate:        Rational,
    /// The HLS codec string, including the H.264 profile and level.
    pub codec:             String,
    /// How the encoded video was created.
    pub processing_method: ProcessingMethod,
    /// The API path for this track's HLS playlist.
    pub playlist_path:     String,
    /// The audio identifiers allowed with this video tier.
    pub audio:             Vec<String>,
}

/// Video metadata without HLS segment details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMedia {
    /// The presentation duration in seconds.
    pub duration_seconds: f64,
    /// The generated video streams.
    pub variants:         Vec<VideoVariant>,
    /// The shared audio streams; their standalone file fields are empty.
    pub audio:            Vec<AudioVariant>,
    /// The API path for the default AAC HLS master playlist.
    pub master_path:      String,
}

/// A recoverable limitation reported in a successful processing result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingWarning {
    /// A stable code, such as `lossless_not_preserved`.
    pub code:    String,
    /// A message for people.
    pub message: String,
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
    /// The standalone audio summary, when this is audio media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio:       Option<AudioMedia>,
    /// The HLS summary, when this is video media.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video:       Option<VideoMedia>,
    /// Recoverable processing limitations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings:    Vec<ProcessingWarning>,
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
    pub max_file_size:                u64,
    /// The number of tasks that can run at the same time, from 1 to 64.
    pub workers:                      usize,
    /// How long finished tasks are kept, in seconds.
    pub task_retention_seconds:       u64,
    /// Limits for image processing.
    pub image_limits:                 ImageLimits,
    /// Audio and video processing settings.
    pub av:                           AvConfig,
    /// How long a playback session is valid, in seconds.
    pub playback_session_seconds:     u64,
    /// How long a completed MP4 export is kept, in seconds.
    pub mp4_export_retention_seconds: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_file_size:                2 * 1024 * 1024 * 1024,
            workers:                      1,
            task_retention_seconds:       7 * 24 * 60 * 60,
            image_limits:                 ImageLimits::default(),
            av:                           AvConfig::default(),
            playback_session_seconds:     24 * 60 * 60,
            mp4_export_retention_seconds: 24 * 60 * 60,
        }
    }
}

/// Settings for the FFmpeg processing executor.
#[derive(Debug, Clone)]
pub struct AvConfig {
    /// The FFmpeg executable.
    pub ffmpeg:          PathBuf,
    /// The ffprobe executable.
    pub ffprobe:         PathBuf,
    /// The 1080p/60 fps bitrate limit in bits per second.
    pub bitrate:         u64,
    /// The maximum number of simultaneous FFmpeg processes.
    pub max_processes:   usize,
    /// The maximum encoder thread count per process.
    pub encoder_threads: usize,
}

impl Default for AvConfig {
    fn default() -> Self {
        Self {
            ffmpeg:          "ffmpeg".into(),
            ffprobe:         "ffprobe".into(),
            bitrate:         12_000_000,
            max_processes:   1,
            encoder_threads: std::thread::available_parallelism()
                .map_or(1, |count| (count.get() / 2).max(1)),
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

/// Options for processing the retained original of existing media.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessOptions {
    /// The output media kind; the default is `Image` for older clients.
    #[serde(skip_serializing_if = "is_image_kind")]
    pub kind:  MediaKind,
    /// The image options.
    pub image: ImageOptions,
    /// The standalone audio options.
    #[serde(skip_serializing_if = "AudioOptions::is_default")]
    pub audio: AudioOptions,
    /// The video options.
    #[serde(skip_serializing_if = "VideoOptions::is_default")]
    pub video: VideoOptions,
}

fn is_image_kind(kind: &MediaKind) -> bool {
    *kind == MediaKind::Image
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            kind:  MediaKind::Image,
            image: ImageOptions::default(),
            audio: AudioOptions::default(),
            video: VideoOptions::default(),
        }
    }
}

/// Options for remuxing an existing video variant into MP4.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mp4ExportOptions {
    /// The identifier of an existing video variant.
    pub variant: String,
}

/// The result of claiming a single-use playback session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackSession {
    /// The secret credential used to read the media and its exports.
    pub token:      String,
    /// The time when this fixed session expires.
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProcessingRecipe {
    pub version:       u32,
    pub image_limits:  ImageLimits,
    pub video_bitrate: u64,
}

impl ProcessingRecipe {
    pub fn from_config(config: &ServiceConfig) -> Self {
        Self {
            version:       1,
            image_limits:  config.image_limits.clone(),
            video_bitrate: config.av.bitrate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HlsInventory {
    pub tracks: Vec<HlsTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HlsTrack {
    pub id:                String,
    pub initialization:    MediaFile,
    pub segments:          Vec<HlsSegment>,
    pub timescale:         u32,
    pub codec:             String,
    pub average_bandwidth: u64,
    pub peak_bandwidth:    u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct HlsSegment {
    pub file:        MediaFile,
    pub start:       i64,
    pub duration:    u64,
    pub independent: bool,
}

pub(super) fn file_references<'a>(
    media: &'a Media,
    inventory: Option<&'a HlsInventory>,
) -> Vec<(String, &'a MediaFile)> {
    let mut files = Vec::new();
    if let Some(file) = &media.original {
        files.push(("original".into(), file));
    }
    files.extend(media.variants.iter().map(|variant| {
        (format!("{}:{}:{}", variant.name, variant.multiplier, variant.format), &variant.file)
    }));
    if let Some(audio) = &media.audio {
        files.extend(audio.variants.iter().filter_map(|variant| {
            variant.file.as_ref().map(|file| (format!("audio:{}", variant.id), file))
        }));
    }
    if let Some(inventory) = inventory {
        for track in &inventory.tracks {
            files.push((format!("hls:{}:init", track.id), &track.initialization));
            files.extend(
                track
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(index, segment)| (format!("hls:{}:{index}", track.id), &segment.file)),
            );
        }
    }
    files
}

pub(super) fn files_mut<'a>(
    media: &'a mut Media,
    inventory: Option<&'a mut HlsInventory>,
) -> impl Iterator<Item = &'a mut MediaFile> {
    media
        .original
        .iter_mut()
        .chain(media.variants.iter_mut().map(|variant| &mut variant.file))
        .chain(media.audio.iter_mut().flat_map(|audio| {
            audio.variants.iter_mut().filter_map(|variant| variant.file.as_mut())
        }))
        .chain(inventory.into_iter().flat_map(|inventory| {
            inventory.tracks.iter_mut().flat_map(|track| {
                std::iter::once(&mut track.initialization)
                    .chain(track.segments.iter_mut().map(|segment| &mut segment.file))
            })
        }))
}

pub(super) fn validate_assets(
    media: &Media,
    inventory: Option<&HlsInventory>,
) -> Result<(), ServiceError> {
    use std::collections::HashSet;
    let invalid = || ServiceError::Invalid("invalid media asset layout".into());
    match media.kind {
        MediaKind::Resource | MediaKind::Image => {
            if media.audio.is_some() || media.video.is_some() || inventory.is_some() {
                return Err(invalid());
            }
        },
        MediaKind::Audio => {
            let audio = media.audio.as_ref().ok_or_else(invalid)?;
            if media.video.is_some()
                || inventory.is_some()
                || !media.variants.is_empty()
                || audio.variants.is_empty()
                || audio.variants.iter().any(|variant| variant.file.is_none())
            {
                return Err(invalid());
            }
        },
        MediaKind::Video => {
            let video = media.video.as_ref().ok_or_else(invalid)?;
            let inventory = inventory.ok_or_else(invalid)?;
            if media.audio.is_some()
                || !media.variants.is_empty()
                || video.variants.is_empty()
                || video.audio.iter().any(|variant| variant.file.is_some())
            {
                return Err(invalid());
            }
            let expected: HashSet<_> = video
                .variants
                .iter()
                .map(|variant| variant.id.as_str())
                .chain(video.audio.iter().map(|variant| variant.id.as_str()))
                .collect();
            let actual: HashSet<_> =
                inventory.tracks.iter().map(|track| track.id.as_str()).collect();
            if expected != actual
                || expected.len() != video.variants.len() + video.audio.len()
                || actual.len() != inventory.tracks.len()
                || inventory.tracks.iter().any(|track| {
                    track.timescale == 0
                        || track.segments.is_empty()
                        || track.segments.iter().any(|segment| segment.duration == 0)
                })
            {
                return Err(invalid());
            }
        },
    }
    let mut roles = HashSet::new();
    for (role, _) in file_references(media, inventory) {
        if !roles.insert(role) {
            return Err(invalid());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) enum Work {
    Upload {
        options: UploadOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recipe:  Option<ProcessingRecipe>,
        #[serde(flatten)]
        input:   StagedInput,
    },
    Process {
        source:     Uuid,
        options:    ProcessOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recipe:     Option<ProcessingRecipe>,
        #[serde(flatten)]
        input:      StagedInput,
        file_name:  String,
        expires_at: Option<DateTime<Utc>>,
    },
    Import {
        hash: String,
    },
    Export(ExportOptions),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StagedInput {
    pub hash:      String,
    // Older tasks did not save the size of their input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
}

#[derive(Debug)]
pub(super) struct PreparedFile {
    pub path:     PathBuf,
    pub metadata: MediaFile,
}
