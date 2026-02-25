use std::{io::ErrorKind, str::FromStr};

use datalith_core::{
    DatalithManager, DatalithResource, DatalithWriteError, FileTypeLevel, mime::Mime,
};
use rocket::{
    Build, Data, Rocket, State,
    http::{ContentType, Status},
    response::content::RawJson,
    serde::uuid::Uuid,
};
use rocket_multipart_form_data::{
    MultipartFormData, MultipartFormDataError, MultipartFormDataField, MultipartFormDataOptions,
};
use serde_json::{Value, json};
use validators::prelude::*;

use super::{Boolean, ServerConfig};
use crate::rocket_mounts::rocket_utils::FileLength;

#[post("/", format = "multipart/form-data", data = "<data>")]
async fn upload(
    server_config: &State<ServerConfig>,
    datalith: &State<DatalithManager>,
    content_type: &ContentType,
    data: Data<'_>,
) -> Result<RawJson<String>, Status> {
    let options = MultipartFormDataOptions {
        max_data_bytes: server_config.max_file_size + 1024,
        allowed_fields: vec![
            MultipartFormDataField::file("file").size_limit(server_config.max_file_size),
            MultipartFormDataField::text("file_name").size_limit(512),
            MultipartFormDataField::text("file_type").size_limit(100),
            MultipartFormDataField::text("temporary").size_limit(5),
        ],
        ..MultipartFormDataOptions::default()
    };

    let multipart_form_data =
        MultipartFormData::parse(content_type, data, options).await.map_err(|err| match err {
            MultipartFormDataError::DataTooLargeError(field) => {
                if field.as_ref() == "file" {
                    Status::PayloadTooLarge
                } else {
                    Status::BadRequest
                }
            },
            _ => Status::BadRequest,
        })?;

    let file_field =
        multipart_form_data.files.get("file").ok_or(Status::BadRequest)?.first().unwrap();

    let file_name = if let Some(file_name) = multipart_form_data.texts.get("file_name") {
        Some(&file_name.first().unwrap().text)
    } else {
        file_field.file_name.as_ref()
    };

    let mime_type = if let Some(file_type) = multipart_form_data.texts.get("file_type") {
        let file_type = file_type.first().unwrap();

        let mime = Mime::from_str(file_type.text.as_str()).map_err(|_| Status::BadRequest)?;

        Some((mime, FileTypeLevel::Manual))
    } else {
        file_field.content_type.clone().map(|e| (e, FileTypeLevel::Fallback))
    };

    let temporary = if let Some(temporary) = multipart_form_data.texts.get("temporary") {
        let temporary = temporary.first().unwrap();

        match Boolean::parse_str(temporary.text.as_str()) {
            Ok(b) => b.0,
            Err(_) => return Err(Status::BadRequest),
        }
    } else {
        false
    };

    match if temporary {
        datalith
            .put_resource_by_path_temporarily(file_field.path.as_path(), file_name, mime_type)
            .await
    } else {
        datalith.put_resource_by_path(file_field.path.as_path(), file_name, mime_type).await
    } {
        Ok(resource) => {
            let value = datalith_resource_to_json_value(resource);

            Ok(RawJson(serde_json::to_string(&value).unwrap()))
        },
        Err(error) => {
            rocket::error!("{error}");

            Err(Status::InternalServerError)
        },
    }
}

#[put("/?<file_name>&<file_type>&<temporary>", data = "<data>")]
#[allow(clippy::too_many_arguments)]
async fn stream_upload(
    server_config: &State<ServerConfig>,
    datalith: &State<DatalithManager>,
    content_type: Option<&ContentType>,
    file_length: Option<&FileLength>,
    file_name: Option<&str>,
    file_type: Option<&str>,
    temporary: Option<Boolean>,
    data: Data<'_>,
) -> Result<RawJson<String>, Status> {
    let content_type = match file_type {
        Some(file_type) => match Mime::from_str(file_type) {
            Ok(mime_type) => Some(mime_type),
            Err(_) => return Err(Status::BadRequest),
        },
        None => match content_type {
            Some(content_type) => match Mime::from_str(&content_type.to_string()) {
                Ok(mime_type) => Some(mime_type),
                Err(_) => return Err(Status::BadRequest),
            },
            None => None,
        },
    };
    let mime_type = content_type.clone().map(|e| (e, FileTypeLevel::Manual));

    let expected_reader_length = validate_content_length(server_config, file_length)?;

    let temporary = temporary.map(|e| e.0).unwrap_or(false);

    // max_file_size plus 1 in order to distinguish the too large payload
    let stream = data.open((server_config.max_file_size + 1).into());

    match if temporary {
        datalith
            .put_resource_by_reader_temporarily(
                stream,
                file_name,
                mime_type,
                Some(expected_reader_length),
            )
            .await
    } else {
        datalith
            .put_resource_by_reader(stream, file_name, mime_type, Some(expected_reader_length))
            .await
    } {
        Ok(resource) => {
            let value = datalith_resource_to_json_value(resource);

            Ok(RawJson(serde_json::to_string(&value).unwrap()))
        },
        Err(DatalithWriteError::FileLengthTooLarge {
            ..
        }) => Err(Status::PayloadTooLarge),
        Err(DatalithWriteError::IOError(error)) if error.kind() == ErrorKind::Other => {
            Err(Status::BadRequest)
        },
        Err(error) => {
            rocket::error!("{error}");

            Err(Status::InternalServerError)
        },
    }
}

#[delete("/<id>")]
async fn delete(datalith: &State<DatalithManager>, id: Uuid) -> Result<&'static str, Status> {
    match datalith.delete_resource_by_id(id).await {
        Ok(true) => Ok("ok"),
        Ok(false) => Err(Status::NotFound),
        Err(error) => {
            rocket::error!("{error}");

            Err(Status::InternalServerError)
        },
    }
}

#[inline]
pub fn mounts(rocket: Rocket<Build>) -> Rocket<Build> {
    rocket.mount("/o", routes![upload, stream_upload, delete])
}

#[inline]
pub fn validate_content_length(
    server_config: &State<ServerConfig>,
    content_length: Option<&FileLength>,
) -> Result<u64, Status> {
    let max_file_size = server_config.max_file_size;

    if let Some(content_length) = content_length {
        let content_length = content_length.to_u64();

        if content_length > max_file_size {
            return Err(Status::PayloadTooLarge);
        }

        Ok(content_length)
    } else {
        Ok(max_file_size)
    }
}

#[inline]
fn datalith_resource_to_json_value(resource: DatalithResource) -> Value {
    json!(
        {
            "id": resource.id().to_string(),
            "created_at": resource.created_at().to_rfc3339(),
            "file_type": resource.file_type().essence_str(),
            "file_size": resource.file().file_size(),
            "file_name": resource.file_name(),
            "is_temporary": resource.file().is_temporary(),
        }
    )
}
