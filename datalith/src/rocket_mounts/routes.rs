use datalith_core::{
    DatalithService, ExportOptions, Media, Mp4ExportOptions, Page, PlaybackSession, ProcessOptions,
    ServiceError, Task, UploadOptions, Uuid,
};
use rocket::{
    Data, State,
    http::{ContentType, Status},
    response::{
        content::{RawHtml, RawJson},
        status::{Accepted, Custom},
    },
    serde::json::Json,
};
use rocket_multipart_form_data::{
    MultipartFormData, MultipartFormDataError, MultipartFormDataField, MultipartFormDataOptions,
    Repetition, multer,
};
use serde_json::Value;
use tokio::fs::File;

use super::{ApiError, IdempotencyKey, ServerConfig};

type TaskResponse = Result<Accepted<Json<Task>>, ApiError>;

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
        allowed_fields,
        ..MultipartFormDataOptions::default()
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
    let reader = File::open(&file.path).await.map_err(ServiceError::from)?;
    let task = service.submit_upload(reader, options, key.0).await?;
    Ok(Accepted(Json(task)))
}

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
    let reader = File::open(&files[0].path).await.map_err(ServiceError::from)?;
    Ok(Accepted(Json(service.submit_import(reader, key.0).await?)))
}

#[post("/exports", format = "json", data = "<options>")]
pub(super) async fn export(
    service: &State<DatalithService>,
    key: IdempotencyKey,
    options: Json<ExportOptions>,
) -> TaskResponse {
    Ok(Accepted(Json(service.submit_export(options.into_inner(), key.0).await?)))
}

#[get("/tasks/<id>")]
pub(super) async fn task(
    service: &State<DatalithService>,
    id: Uuid,
) -> Result<Json<Task>, ApiError> {
    Ok(Json(service.get_task(id).await?.ok_or(ServiceError::NotFound)?))
}

#[post("/tasks/<id>/cancel?<session>")]
pub(super) async fn cancel(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
) -> TaskResponse {
    Ok(Accepted(Json(service.cancel_task_with_session(id, session).await?)))
}

#[post("/tasks/<id>/retry?<session>")]
pub(super) async fn retry(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
) -> TaskResponse {
    Ok(Accepted(Json(service.retry_task_with_session(id, session).await?)))
}

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

#[get("/media/<id>")]
pub(super) async fn media(
    service: &State<DatalithService>,
    id: Uuid,
) -> Result<Json<Media>, ApiError> {
    Ok(Json(service.get_media(id).await?.ok_or(ServiceError::NotFound)?))
}

#[post("/media/<id>/tasks", format = "json", data = "<options>")]
pub(super) async fn process(
    service: &State<DatalithService>,
    id: Uuid,
    key: IdempotencyKey,
    options: Json<ProcessOptions>,
) -> TaskResponse {
    Ok(Accepted(Json(service.submit_process(id, options.into_inner(), key.0).await?)))
}

#[delete("/media/<id>")]
pub(super) async fn delete(service: &State<DatalithService>, id: Uuid) -> Result<Status, ApiError> {
    if service.delete_media(id).await? {
        Ok(Status::NoContent)
    } else {
        Err(ServiceError::NotFound.into())
    }
}

#[get("/capabilities")]
pub(super) fn capabilities(service: &State<DatalithService>) -> Json<Value> {
    Json(service.capabilities())
}

#[get("/openapi.json")]
pub(super) fn openapi() -> RawJson<&'static str> {
    RawJson(include_str!("../openapi.json"))
}

#[post("/media/<id>/playback-sessions")]
pub(super) async fn playback_session(
    service: &State<DatalithService>,
    id: Uuid,
    key: IdempotencyKey,
) -> Result<Custom<Json<PlaybackSession>>, ApiError> {
    Ok(Custom(Status::Created, Json(service.claim_playback_session(id, key.0).await?)))
}

#[get("/player")]
pub(super) fn player() -> RawHtml<&'static str> {
    RawHtml(include_str!("../player.html"))
}

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
