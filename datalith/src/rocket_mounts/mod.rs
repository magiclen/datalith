mod content;
mod openapi;
mod routes;

#[cfg(test)]
mod tests;

use std::{io::Cursor, net::IpAddr};

use datalith_core::{DatalithService, ServiceError, Uuid, chrono::Utc};
use rocket::{
    Build, Config, Request, Response, Rocket,
    fairing::AdHoc,
    http::{ContentType, Status},
    request::{FromRequest, Outcome},
    response::{self, Responder},
};
use serde_json::json;
use utoipa_swagger_ui::{Config as SwaggerConfig, SwaggerUi};

fn swagger_config() -> SwaggerConfig<'static> {
    SwaggerConfig::new(["json"]).validator_url("none")
}

#[derive(Debug)]
pub(crate) struct ServerConfig {
    max_file_size: u64,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status:        Status,
    code:          &'static str,
    message:       String,
    content_range: Option<String>,
}

impl ApiError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            status:        Status::BadRequest,
            code:          "invalid_request",
            message:       message.into(),
            content_range: None,
        }
    }

    pub(crate) fn range(size: u64) -> Self {
        Self {
            status:        Status::RangeNotSatisfiable,
            code:          "range_not_satisfiable",
            message:       "The requested byte range is not available.".into(),
            content_range: Some(format!("bytes */{size}")),
        }
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        let status = match &error {
            ServiceError::Invalid(_) => Status::BadRequest,
            ServiceError::NotFound => Status::NotFound,
            ServiceError::Conflict(_) | ServiceError::Cancelled => Status::Conflict,
            ServiceError::Busy => Status::ServiceUnavailable,
            ServiceError::Unsupported(_) => Status::UnsupportedMediaType,
            ServiceError::PayloadTooLarge => Status::PayloadTooLarge,
            _ => Status::InternalServerError,
        };
        let code = error.code();
        let message = if status == Status::InternalServerError {
            rocket::error!("{error}");
            "The service could not complete the request.".into()
        } else {
            error.to_string()
        };
        Self {
            status,
            code,
            message,
            content_range: None,
        }
    }
}

impl<'r> Responder<'r, 'static> for ApiError {
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'static> {
        let body = json!({ "error": { "code": self.code, "message": self.message }, "request_id": request_id(request) }).to_string();
        let mut response = Response::build();
        response
            .status(self.status)
            .header(ContentType::JSON)
            .raw_header("Cache-Control", "no-store");
        if self.status == Status::ServiceUnavailable {
            response.raw_header("Retry-After", "1");
        }
        if let Some(value) = self.content_range {
            response.raw_header("Content-Range", value);
        }
        response.sized_body(body.len(), Cursor::new(body)).ok()
    }
}

#[derive(Debug)]
pub(crate) struct IdempotencyKey(Option<String>);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for IdempotencyKey {
    type Error = &'static str;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let mut values = request.headers().get("Idempotency-Key");
        let value = values.next();
        if values.next().is_some()
            || value.is_some_and(|v| {
                v.is_empty()
                    || v.len() > 128
                    || !v.is_ascii()
                    || v.bytes().any(|b| b.is_ascii_control())
            })
        {
            return Outcome::Error((Status::BadRequest, "Invalid Idempotency-Key"));
        }
        Outcome::Success(Self(value.map(str::to_owned)))
    }
}

fn request_id(request: &Request<'_>) -> String {
    request.local_cache(Uuid::new_v4).to_string()
}

#[catch(default)]
fn error_catcher(status: Status, _: &Request<'_>) -> ApiError {
    ApiError {
        status,
        code: match status.code {
            404 => "not_found",
            413 => "payload_too_large",
            415 => "unsupported_media_type",
            422 | 400 => "invalid_request",
            _ => "http_error",
        },
        message: status.reason_lossy().to_owned(),
        content_range: None,
    }
}

pub fn create(address: IpAddr, port: u16, max_file_size: u64) -> Rocket<Build> {
    let figment = Config::figment()
        .merge(("ident", "Datalith"))
        .merge(("address", address))
        .merge(("port", port))
        .merge(("limits.json", 1024 * 1024));
    rocket::custom(figment)
        .manage(ServerConfig {
            max_file_size,
        })
        .register("/", catchers![error_catcher])
        .attach(AdHoc::on_response("Response metadata", |request, response| {
            Box::pin(async move {
                response.set_raw_header("X-Request-Id", request_id(request));
                if response.headers().get_one("Cache-Control").is_none() {
                    response.set_raw_header("Cache-Control", "no-store");
                }
                response.set_raw_header(
                    "Date",
                    Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                );
            })
        }))
        .attach(AdHoc::on_shutdown("Stop task workers", |rocket| {
            Box::pin(async move {
                if let Some(service) = rocket.state::<DatalithService>()
                    && let Err(error) = service.close().await
                {
                    rocket::error!("Could not close Datalith: {error}");
                }
            })
        }))
        .mount("/api/v1", routes![
            routes::upload,
            routes::import,
            routes::export,
            routes::task,
            routes::cancel,
            routes::retry,
            routes::media_list,
            routes::media,
            routes::process,
            routes::delete,
            routes::capabilities,
            routes::openapi,
            routes::legacy_openapi,
            routes::docs,
            routes::player,
            routes::playback_session,
            routes::mp4_export,
            content::hls_master,
            content::head_hls_master,
            content::hls_track,
            content::head_hls_track,
            content::hls_asset,
            content::head_hls_asset,
            content::get_content,
            content::head_content,
            content::get_artifact,
            content::head_artifact,
        ])
        .mount("/", SwaggerUi::new("/api/v1/docs/<_..>").config(swagger_config()))
}
