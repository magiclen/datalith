use std::time::Duration;

use datalith_core::{
    DatalithService, ExportOptions, Media, Mp4ExportOptions, Page, PlaybackSession, ProcessOptions,
    ServiceError, Task, UploadOptions, Uuid,
};
use rocket::{
    Data, Either, State,
    http::{ContentType, Status, uri::Origin},
    response::{
        Redirect,
        content::RawHtml,
        status::{Accepted, Custom},
    },
    serde::json::Json,
};
use rocket_multipart_form_data::{
    MultipartFormData, MultipartFormDataError, MultipartFormDataField, MultipartFormDataOptions,
    Repetition, multer,
};
use serde_json::Value;

use super::{ApiError, IdempotencyKey, ServerConfig};

type TaskResponse = Result<Accepted<Json<Task>>, ApiError>;

const BUSY_RETRY_DELAY: Duration = Duration::from_millis(100);
const BUSY_RETRY_LIMIT: u32 = 100;

// The received file is already on disk, so wait for an export snapshot instead of making the client send it again.
async fn submit_received<F: Future<Output = Result<Task, ServiceError>>>(
    mut submit: impl FnMut() -> F,
) -> Result<Task, ServiceError> {
    let mut retries = 0;
    loop {
        match submit().await {
            Err(ServiceError::Busy) if retries < BUSY_RETRY_LIMIT => {
                retries += 1;
                tokio::time::sleep(BUSY_RETRY_DELAY).await;
            },
            result => return result,
        }
    }
}

async fn multipart(
    content_type: &ContentType,
    data: Data<'_>,
    config: &ServerConfig,
    with_options: bool,
) -> Result<MultipartFormData, ApiError> {
    let mut allowed_fields = vec![
        MultipartFormDataField::file("file")
            .size_limit(config.max_file_size)
            .repetition(Repetition::fixed(2)),
    ];
    if with_options {
        allowed_fields.push(
            MultipartFormDataField::text("options")
                .size_limit(1024 * 1024)
                .repetition(Repetition::fixed(2)),
        );
    }
    let options = MultipartFormDataOptions {
        max_data_bytes: config.max_file_size.saturating_add(1024 * 1024 + 64 * 1024),
        temporary_dir: config.temporary_directory.clone(),
        allowed_fields,
    };
    MultipartFormData::parse(content_type, data, options).await.map_err(|error| match error {
        MultipartFormDataError::DataTooLargeError(_)
        | MultipartFormDataError::MulterError(
            multer::Error::StreamSizeExceeded {
                ..
            }
            | multer::Error::FieldSizeExceeded {
                ..
            },
        ) => ServiceError::PayloadTooLarge.into(),
        MultipartFormDataError::IOError(error) => ServiceError::Io(error).into(),
        _ => ApiError::invalid("The multipart body must contain one file and valid options."),
    })
}

#[utoipa::path(
    post,
    path = "/uploads",
    operation_id = "upload",
    summary = "Save an upload and create a task",
    tag = "Tasks",
    description = "The upload streams to disk and returns a task before conversion. Enable automatic conversion with enable_convert_to_image, enable_convert_to_audio, or enable_convert_to_video, using a resource kind or no kind. Only enabled matching types are converted; other files stay resources. All enabled settings are checked first, and video conversion requires resolution and fps pairs. Automatic tasks have kind upload; their result reports the detected media kind. Explicit image, audio, or video kinds cannot use automatic flags. Without options the file stays a permanent resource. Image originals are kept by default; audio and video originals are not.",
    request_body(content = super::openapi::UploadBody, content_type = "multipart/form-data", encoding(("options" = (content_type = "application/json")))),
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409.")
    ),
    responses(
        (status = 202, description = "Save an upload and create a task", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/uploads", format = "multipart/form-data", data = "<data>")]
pub(super) async fn upload(
    service: &State<DatalithService>,
    config: &State<ServerConfig>,
    key: IdempotencyKey,
    content_type: &ContentType,
    data: Data<'_>,
) -> TaskResponse {
    let form = multipart(content_type, data, config, true).await?;
    let files =
        form.files.get("file").ok_or_else(|| ApiError::invalid("The file field is required."))?;
    if files.len() != 1 {
        return Err(ApiError::invalid("Exactly one file is required."));
    }
    let file = &files[0];
    let mut options: UploadOptions = match form.texts.get("options") {
        Some(values) if values.len() == 1 => serde_json::from_str(&values[0].text)
            .map_err(|error| ApiError::invalid(format!("Invalid options: {error}")))?,
        Some(_) => return Err(ApiError::invalid("Only one options field is allowed.")),
        None => UploadOptions::default(),
    };
    if options.file_name.is_none() {
        options.file_name.clone_from(&file.file_name);
    }
    // Clients often send `application/octet-stream` for any file, so let the service detect the type instead.
    if options.file_type.is_none() {
        options.file_type = file
            .content_type
            .as_ref()
            .filter(|mime| mime.essence_str() != "application/octet-stream")
            .map(ToString::to_string);
    }
    let task =
        submit_received(|| service.submit_upload_file(&file.path, options.clone(), key.0.clone()))
            .await?;
    Ok(Accepted(Json(task)))
}

#[utoipa::path(
    post,
    path = "/imports",
    operation_id = "importMedia",
    summary = "Check and import a Datalith archive",
    tag = "Tasks",
    description = "Import keeps IDs where possible and skips matching items. An ID used by different data is replaced with a new ID. The result includes the old-to-new ID maps. Importing the same archive again does not create duplicates. Version 2 archives include audio and HLS contents; version 1 archives remain supported.",
    request_body(content = super::openapi::ImportBody, content_type = "multipart/form-data"),
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409.")
    ),
    responses(
        (status = 202, description = "Check and import a Datalith archive", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/imports", format = "multipart/form-data", data = "<data>")]
pub(super) async fn import(
    service: &State<DatalithService>,
    config: &State<ServerConfig>,
    key: IdempotencyKey,
    content_type: &ContentType,
    data: Data<'_>,
) -> TaskResponse {
    let form = multipart(content_type, data, config, false).await?;
    let files =
        form.files.get("file").ok_or_else(|| ApiError::invalid("The file field is required."))?;
    if files.len() != 1 {
        return Err(ApiError::invalid("Exactly one file is required."));
    }
    let task =
        submit_received(|| service.submit_import_file(&files[0].path, key.0.clone())).await?;
    Ok(Accepted(Json(task)))
}

#[utoipa::path(
    post,
    path = "/exports",
    operation_id = "exportMedia",
    summary = "Export all media or selected IDs",
    tag = "Tasks",
    description = "Export briefly pauses content writes, file cleanup, and single-use claims while it records the selected media, then writes the archive from that snapshot. Requests that arrive during the pause get 503 with Retry-After; uploads already being saved finish and are queued. Normal downloads continue. Archive version 2 contains a manifest, complete HLS inventory, and one copy of each unique file content, including originals, image and standalone audio outputs, and HLS initialization files and segments. Sessions, task history, settings, and temporary MP4 artifacts are not transferred.",
    request_body = datalith_core::ExportOptions,
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409.")
    ),
    responses(
        (status = 202, description = "Export all media or selected IDs", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/exports", format = "json", data = "<options>")]
pub(super) async fn export(
    service: &State<DatalithService>,
    key: IdempotencyKey,
    options: Json<ExportOptions>,
) -> TaskResponse {
    Ok(Accepted(Json(service.submit_export(options.into_inner(), key.0).await?)))
}

#[utoipa::path(
    get,
    path = "/tasks/{id}",
    operation_id = "getTask",
    summary = "Read task state and result",
    tag = "Tasks",
    params(
        ("id" = datalith_core::Uuid, Path)
    ),
    responses(
        (status = 200, description = "Read task state and result", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[get("/tasks/<id>")]
pub(super) async fn task(
    service: &State<DatalithService>,
    id: Uuid,
) -> Result<Json<Task>, ApiError> {
    Ok(Json(service.get_task(id).await?.ok_or(ServiceError::NotFound)?))
}

#[utoipa::path(
    post,
    path = "/tasks/{id}/cancel",
    operation_id = "cancelTask",
    summary = "Ask a task to stop between processing steps",
    tag = "Tasks",
    description = "MP4 tasks from single-use video require their active session credential even when the same task is requested again.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("session" = Option<String>, Query, description = "Required for an MP4 task created from single-use video, using the same still-valid playback credential. Other tasks do not require it.")
    ),
    responses(
        (status = 202, description = "Ask a task to stop between processing steps", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/tasks/<id>/cancel?<session>")]
pub(super) async fn cancel(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
) -> TaskResponse {
    Ok(Accepted(Json(service.cancel_task_with_session(id, session).await?)))
}

#[utoipa::path(
    post,
    path = "/tasks/{id}/retry",
    operation_id = "retryTask",
    summary = "Retry a failed or cancelled task",
    tag = "Tasks",
    description = "MP4 tasks from single-use video require their active session credential even when the same task is requested again.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("session" = Option<String>, Query, description = "Required for an MP4 task created from single-use video, using the same still-valid playback credential. Other tasks do not require it.")
    ),
    responses(
        (status = 202, description = "Retry a failed or cancelled task", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/tasks/<id>/retry?<session>")]
pub(super) async fn retry(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
) -> TaskResponse {
    Ok(Accepted(Json(service.retry_task_with_session(id, session).await?)))
}

#[utoipa::path(
    get,
    path = "/media",
    operation_id = "listMedia",
    summary = "List available media",
    tag = "Media",
    params(
        ("page" = Option<u64>, Query, minimum = 1, example = 1),
        ("per_page" = Option<u64>, Query, minimum = 1, maximum = 100, example = 50)
    ),
    responses(
        (status = 200, description = "List available media", body = super::openapi::MediaPage, content_type = "application/json")
    )
)]
#[get("/media?<page>&<per_page>")]
pub(super) async fn media_list(
    service: &State<DatalithService>,
    page: Option<u64>,
    per_page: Option<u64>,
) -> Result<Json<Page<Media>>, ApiError> {
    let page = page.unwrap_or(1);
    let per_page = per_page.unwrap_or(50);
    if page == 0 || per_page == 0 || per_page > 100 {
        return Err(ApiError::invalid(
            "The page must be positive and per_page must be between 1 and 100.",
        ));
    }
    Ok(Json(service.list_media(page, per_page).await?))
}

#[utoipa::path(
    get,
    path = "/media/{id}",
    operation_id = "getMedia",
    summary = "Read metadata without using a single-use download claim",
    description = "Read metadata before claiming playback, or pass the valid playback session to read claimed single-use audio/video metadata. Requests without a session keep hiding consumed media.",
    tag = "Media",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("session" = Option<String>, Query, description = "An active playback session for claimed single-use audio/video metadata.")
    ),
    responses(
        (status = 200, description = "Read metadata without using a single-use download claim", body = datalith_core::Media, content_type = "application/json")
    )
)]
#[get("/media/<id>?<session>")]
pub(super) async fn media(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
) -> Result<Json<Media>, ApiError> {
    Ok(Json(service.get_media_with_session(id, session).await?.ok_or(ServiceError::NotFound)?))
}

#[utoipa::path(
    post,
    path = "/media/{id}/tasks",
    operation_id = "processMedia",
    summary = "Create media from a retained original",
    tag = "Tasks",
    description = "Create new image, audio, or video media from a retained original without changing the source. The kind field is required and selects image, audio, or video. The original must exist. Single-use sources return 409. The result keeps the source expiry and is not published after it has expired. The same idempotency key, source ID, and options return the existing task even after the source is deleted or expires.",
    request_body = datalith_core::ProcessOptions,
    params(
        ("id" = datalith_core::Uuid, Path),
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409.")
    ),
    responses(
        (status = 202, description = "Create media from a retained original", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/media/<id>/tasks", format = "json", data = "<options>")]
pub(super) async fn process(
    service: &State<DatalithService>,
    id: Uuid,
    key: IdempotencyKey,
    options: Json<ProcessOptions>,
) -> TaskResponse {
    Ok(Accepted(Json(service.submit_process(id, options.into_inner(), key.0).await?)))
}

#[utoipa::path(
    delete,
    path = "/media/{id}",
    operation_id = "deleteMedia",
    summary = "Delete media and clean up unused files later",
    tag = "Media",
    params(
        ("id" = datalith_core::Uuid, Path)
    ),
    responses(
        (status = 204, description = "Delete media and clean up unused files later")
    )
)]
#[delete("/media/<id>")]
pub(super) async fn delete(service: &State<DatalithService>, id: Uuid) -> Result<Status, ApiError> {
    if service.delete_media(id).await? {
        Ok(Status::NoContent)
    } else {
        Err(ServiceError::NotFound.into())
    }
}

#[utoipa::path(
    get,
    path = "/capabilities",
    operation_id = "getCapabilities",
    summary = "Read processing availability, profiles, and limits",
    tag = "Service",
    description = "Use media.audio and media.video for new upload and reprocessing availability. The av object separates external tool discovery from AAC, H.264, and FLAC encoder availability and reports the minimum tool major version and discovery failure reason. video.mp4_export follows usable tool availability and can be true without an H.264 encoder. Existing media, sessions, and completed artifact reads remain available without processing tools.",
    responses(
        (status = 200, description = "Compiled features, installed tool availability, output profile names, and configured limits.", body = super::openapi::Capabilities, content_type = "application/json")
    )
)]
#[get("/capabilities")]
pub(super) fn capabilities(service: &State<DatalithService>) -> Json<Value> {
    Json(service.capabilities())
}

#[utoipa::path(
    get,
    path = "/docs/json",
    operation_id = "getOpenApi",
    summary = "Read this OpenAPI document",
    tag = "Service",
    responses(
        (status = 200, description = "OpenAPI 3.1 document", body = Object, content_type = "application/json")
    )
)]
#[get("/docs/json")]
pub(super) fn openapi() -> Json<&'static utoipa::openapi::OpenApi> {
    Json(super::openapi::document())
}

#[get("/docs")]
pub(super) fn docs(
    uri: &Origin<'_>,
) -> Result<Either<Redirect, utoipa_swagger_ui::SwaggerFile<'static>>, ApiError> {
    if !uri.path().as_str().ends_with('/') {
        return Ok(Either::Left(Redirect::permanent("docs/")));
    }
    let file = utoipa_swagger_ui::serve("", std::sync::Arc::new(super::swagger_config()))
        .map_err(|error| ServiceError::Internal(error.to_string()))?
        .ok_or(ServiceError::NotFound)?;
    Ok(Either::Right(file))
}

#[utoipa::path(
    post,
    path = "/media/{id}/playback-sessions",
    operation_id = "claimPlaybackSession",
    summary = "Claim single-use audio or video playback",
    tag = "Playback",
    description = "Claim one fixed playback session and return its secret token. Read metadata before claiming because ordinary metadata reads and lists hide consumed media. The same idempotency key retries the original session. The token permits repeated content, original, HLS, and MP4 requests until fixed expiry, at most the media expiry. Deleting media invalidates it. Ordinary media do not require a session. The first reply and idempotent retries use the same millisecond-precision expiry.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409.")
    ),
    responses(
        (status = 201, description = "The claimed playback session.", body = datalith_core::PlaybackSession, content_type = "application/json", headers(("Cache-Control" = String, description = "no-store for the secret session response.")))
    )
)]
#[post("/media/<id>/playback-sessions")]
pub(super) async fn playback_session(
    service: &State<DatalithService>,
    id: Uuid,
    key: IdempotencyKey,
) -> Result<Custom<Json<PlaybackSession>>, ApiError> {
    Ok(Custom(Status::Created, Json(service.claim_playback_session(id, key.0).await?)))
}

#[utoipa::path(
    get,
    path = "/player",
    operation_id = "getPlayerExample",
    summary = "Open the minimal browser playback example",
    tag = "Playback",
    description = "A same-origin browser example for HLS playback, adaptive video selection, and supported AAC/FLAC choices. It is not a formal client SDK.",
    responses(
        (status = 200, description = "Browser example page.", body = String, content_type = "text/html")
    )
)]
#[get("/player")]
pub(super) fn player() -> RawHtml<&'static str> {
    RawHtml(include_str!("../player.html"))
}

#[utoipa::path(
    post,
    path = "/media/{id}/mp4-exports",
    operation_id = "exportMp4",
    summary = "Remux an existing video version into MP4",
    tag = "Tasks",
    description = "Select an existing video identifier and the best allowed available audio in order FLAC, higher AAC, then lower AAC. A source with no audio produces video-only MP4. Existing compressed streams are copied and faststart is written; no stream is re-encoded. The task has kind mp4_export. New remuxing requires av-convert and usable FFmpeg and ffprobe version 9 or later; existing HLS, audio, sessions, and completed artifacts remain readable without that processing capability.",
    request_body = datalith_core::Mp4ExportOptions,
    params(
        ("id" = datalith_core::Uuid, Path),
        ("Idempotency-Key" = Option<String>, Header, description = "An ASCII key stored with the task for seven days by default. The same key and input return the existing task. Different input with the same key returns 409."),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video. Ordinary media need no token.")
    ),
    responses(
        (status = 202, description = "The queued MP4 export task.", body = datalith_core::Task, content_type = "application/json")
    )
)]
#[post("/media/<id>/mp4-exports?<session>", format = "json", data = "<options>")]
pub(super) async fn mp4_export(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
    key: IdempotencyKey,
    options: Json<Mp4ExportOptions>,
) -> TaskResponse {
    Ok(Accepted(Json(service.submit_mp4_export(id, options.into_inner(), session, key.0).await?)))
}
