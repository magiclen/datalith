use std::{collections::BTreeMap, sync::OnceLock};

use datalith_core::{
    ImageLimits, Media, MediaFile, Mp4ExportResult, Page, ProcessingMode, TaskError, Uuid,
};
use serde::{Deserialize, Serialize};
use utoipa::{
    Modify, OpenApi, PartialSchema, ToSchema,
    openapi::{self, RefOr},
};

use super::{content, routes};

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesMedia {
    resource: bool,
    image:    bool,
    audio:    bool,
    video:    bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesImage {
    engine:                   String,
    animated_inputs:          Vec<String>,
    outputs:                  Vec<String>,
    apng_requires_ffmpeg:     bool,
    apng_timing_precision_ms: u64,
    limits:                   ImageLimits,
    processing_modes:         Vec<ProcessingMode>,
    save_original_default:    bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesAv {
    /// The external FFmpeg and ffprobe programs passed discovery and the minimum-version checks.
    /// This does not imply all encoders are available.
    available:          bool,
    minimum_tool_major: u64,
    /// AAC encoding is available for new standalone audio processing.
    audio_encoder:      bool,
    /// H.264 encoding and its required timestamp support are available for new video processing.
    video_encoder:      bool,
    /// FLAC encoding is available.
    /// Lossless preservation can fall back to AAC with a warning when it is needed but absent.
    flac_encoder:       bool,
    /// Reason external processing discovery is unavailable, or null when the tool discovery succeeded.
    #[schema(required = true)]
    unavailable_reason: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesAudio {
    engine:                 String,
    profiles:               Vec<String>,
    aac_sample_rate:        u64,
    /// AAC targets in bits per second: lossless sources use 256000; lossy sources can fall back to 128000 after a payload comparison.
    aac_bitrates:           Vec<u64>,
    mp3_fallback:           bool,
    save_original_default:  bool,
    processing_modes:       Vec<ProcessingMode>,
    selected_audio_streams: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesVideo {
    engine:                String,
    codec:                 String,
    pixel_format:          String,
    delivery:              String,
    requires_variants:     bool,
    resolution_tiers:      Vec<u64>,
    frame_rate_tiers:      Vec<u64>,
    /// Global 1080p/60 fps rate limit, in bits per second.
    bitrate:               u64,
    bitrate_unit:          String,
    save_original_default: bool,
    processing_modes:      Vec<ProcessingMode>,
    /// New MP4 remux tasks can run when external tools are available, even if the H.264 encoder is not.
    /// This does not control reading completed artifacts.
    mp4_export:            bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilitiesPlaybackSessions {
    /// Configured fixed playback-session lifetime, capped by media expiry.
    seconds:              u64,
    single_use_semantics: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Capabilities {
    api_version:                  String,
    version:                      String,
    /// Availability for new uploads and reprocessing.
    /// Existing media reads do not depend on these processing flags.
    media:                        CapabilitiesMedia,
    image:                        CapabilitiesImage,
    /// A decimal string that JavaScript can read with BigInt.
    max_file_size:                String,
    task_retention_seconds:       u64,
    task_notifications:           Vec<String>,
    cancellation:                 String,
    /// Current export archive format version, 2.
    /// Import also accepts version 1.
    archive_version:              u64,
    export_pauses_writes:         bool,
    av:                           CapabilitiesAv,
    /// Audio recipe metadata; use media.audio and av encoder flags for actual processing availability.
    audio:                        CapabilitiesAudio,
    /// Video recipe metadata.
    /// MP4 remux availability is independent of the video encoder flag.
    video:                        CapabilitiesVideo,
    playback_sessions:            CapabilitiesPlaybackSessions,
    /// Configured completed MP4 artifact retention, independent of task-history retention.
    mp4_export_retention_seconds: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ExportResult {
    artifact_path: String,
    media_count:   u64,
    artifact:      MediaFile,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ImportResult {
    imported:    u64,
    skipped:     u64,
    id_map:      BTreeMap<String, Uuid>,
    archive_id:  Uuid,
    file_id_map: BTreeMap<String, Uuid>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Error {
    error:      TaskError,
    request_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub(super) struct UploadBody {
    #[schema(value_type = String, format = Binary)]
    file:    Vec<u8>,
    #[schema(nullable = false)]
    options: Option<datalith_core::UploadOptions>,
}

#[derive(Serialize, ToSchema)]
pub(super) struct ImportBody {
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
}

#[derive(Serialize, ToSchema)]
#[schema(value_type = String, format = Binary)]
pub(super) struct Binary(Vec<u8>);

#[derive(Serialize, ToSchema)]
pub(super) struct MediaPage(Page<Media>);

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub(super) enum TaskResult {
    Media(Media),
    Export(ExportResult),
    Import(ImportResult),
    Mp4Export(Mp4ExportResult),
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum HlsAudio {
    Aac,
    All,
    Flac,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum ContentFormat {
    Webp,
    Png,
    Jpeg,
    Gif,
    M4a,
    Aac,
    Flac,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Datalith API", version = "1.0.0", description = "A media store with files on disk, metadata and tasks in SQLite, image variants, AAC and FLAC audio, and H.264 HLS video. Uploads return durable tasks before conversion finishes. IDs are UUID strings, dates are UTC RFC 3339 strings, and file sizes and page totals are decimal strings. Authentication belongs to the trusted backend or reverse proxy.", license(name = "MIT", identifier = "MIT")),
    servers((url = "..", description = "API root relative to this document.")),
    paths(
        routes::upload, routes::import, routes::export, routes::task, routes::cancel,
        routes::retry, routes::media_list, routes::media, routes::process, routes::delete,
        routes::capabilities, routes::openapi, routes::player, routes::playback_session,
        routes::mp4_export, content::get_content, content::head_content,
        content::get_artifact, content::head_artifact, content::hls_master,
        content::head_hls_master, content::hls_track, content::head_hls_track,
        content::hls_asset, content::head_hls_asset
    ),
    components(schemas(Error, TaskResult, HlsAudio, ContentFormat)),
    modifiers(&Documentation)
)]
pub(super) struct ApiDoc;

pub(super) fn document() -> &'static openapi::OpenApi {
    static DOCUMENT: OnceLock<openapi::OpenApi> = OnceLock::new();
    DOCUMENT.get_or_init(ApiDoc::openapi)
}

struct Documentation;

impl Modify for Documentation {
    fn modify(&self, document: &mut openapi::OpenApi) {
        use openapi::{
            ContentBuilder, Ref, Required, ResponseBuilder,
            header::HeaderBuilder,
            path::{ParameterBuilder, ParameterIn},
            schema::{AnyOfBuilder, ObjectBuilder, Schema, Type},
        };
        document.openapi = openapi::OpenApiVersion::Version31;
        let schemas = &mut document.components.as_mut().expect("API schemas are generated").schemas;
        if let Some(RefOr::T(Schema::Object(task))) = schemas.get_mut("Task") {
            task.properties.insert(
                "result".into(),
                AnyOfBuilder::new()
                    .item(Ref::from_schema_name("TaskResult"))
                    .item(ObjectBuilder::new().schema_type(Type::Null))
                    .into(),
            );
        }

        // Both canonical HLS asset paths use the same file-name handlers.
        let assets = document
            .paths
            .paths
            .remove("/media/{id}/hls/{track}/{name}")
            .expect("HLS asset handlers are documented");
        for (suffix, operation) in
            [("init.mp4", "HlsInitialization"), ("segment-{sequence}.m4s", "HlsSegment")]
        {
            let mut path = assets.clone();
            for (method, entry) in [("get", &mut path.get), ("head", &mut path.head)] {
                if let Some(entry) = entry {
                    entry.operation_id = Some(format!("{method}{operation}"));
                    if let Some(parameters) = &mut entry.parameters {
                        parameters.retain(|parameter| match parameter {
                            RefOr::T(parameter) => parameter.name != "name",
                            RefOr::Ref(_) => true,
                        });
                        if suffix.starts_with("segment-") {
                            parameters.push(
                                ParameterBuilder::new()
                                    .name("sequence")
                                    .parameter_in(ParameterIn::Path)
                                    .required(Required::True)
                                    .schema(Some(u64::schema()))
                                    .build()
                                    .into(),
                            );
                        }
                    }
                }
            }
            document.paths.paths.insert(format!("/media/{{id}}/hls/{{track}}/{suffix}"), path);
        }

        for path in document.paths.paths.values_mut() {
            for (head, operation) in [
                (false, &mut path.get),
                (false, &mut path.post),
                (false, &mut path.delete),
                (true, &mut path.head),
            ] {
                let Some(operation) = operation else { continue };
                for (status, description) in [
                    ("400", "Invalid request."),
                    ("404", "The requested item is not available."),
                    ("409", "The request conflicts with stored state."),
                    ("413", "The request is too large."),
                    ("415", "The media or processing feature is not supported."),
                    ("422", "Invalid request parameters."),
                    ("500", "The service could not complete the request."),
                    ("503", "Writes are paused; retry later."),
                ] {
                    operation.responses.responses.entry(status.into()).or_insert_with(|| {
                        ResponseBuilder::new()
                            .description(description)
                            .content(
                                "application/json",
                                ContentBuilder::new()
                                    .schema(Some(Ref::from_schema_name("Error")))
                                    .build(),
                            )
                            .build()
                            .into()
                    });
                }
                for (status, response) in &mut operation.responses.responses {
                    if let RefOr::T(response) = response {
                        if head {
                            response.content.clear();
                        }
                        response.headers.insert(
                            "X-Request-Id".into(),
                            HeaderBuilder::new()
                                .description(Some("Request ID created by the server."))
                                .schema(Some(
                                    ObjectBuilder::new().schema_type(Type::String).format(Some(
                                        openapi::schema::SchemaFormat::KnownFormat(
                                            openapi::schema::KnownFormat::Uuid,
                                        ),
                                    )),
                                ))
                                .build()
                                .into(),
                        );
                        if status == "503" {
                            response.headers.insert(
                                "Retry-After".into(),
                                HeaderBuilder::new()
                                    .description(Some("Delay before retrying, in seconds."))
                                    .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
                                    .build()
                                    .into(),
                            );
                        }
                    }
                }
            }
        }
    }
}
