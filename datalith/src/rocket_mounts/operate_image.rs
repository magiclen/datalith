use datalith_core::{
    CenterCrop, DatalithImage, DatalithImageWriteError, DatalithManager, DatalithWriteError, Uuid,
};
use rocket::{
    http::{ContentType, Status},
    response::content::RawJson,
    Build, Data, Rocket, State,
};
use rocket_multipart_form_data::{
    MultipartFormData, MultipartFormDataError, MultipartFormDataField, MultipartFormDataOptions,
};
use serde_json::{json, Value};
use validators::prelude::*;

use super::{Boolean, ServerConfig};
use crate::rocket_mounts::{operate::validate_content_length, rocket_utils::FileLength};

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
            MultipartFormDataField::text("max_width").size_limit(10),
            MultipartFormDataField::text("max_height").size_limit(10),
            MultipartFormDataField::text("center_crop").size_limit(30),
            MultipartFormDataField::text("save_original_file").size_limit(5),
        ],
        ..MultipartFormDataOptions::default()
    };

    let mut multipart_form_data =
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

    let file_name = if let Some(mut file_name) = multipart_form_data.texts.remove("file_name") {
        Some(file_name.remove(0).text)
    } else {
        file_field.file_name.clone()
    };

    let max_width: Option<u16> = if let Some(max_width) = multipart_form_data.texts.get("max_width")
    {
        Some(max_width[0].text.parse().map_err(|_| Status::BadRequest)?)
    } else {
        None
    };

    let max_height: Option<u16> =
        if let Some(max_height) = multipart_form_data.texts.get("max_height") {
            Some(max_height[0].text.parse().map_err(|_| Status::BadRequest)?)
        } else {
            None
        };

    let center_crop = multipart_form_data.texts.get("center_crop").map(|v| v[0].text.as_str());
    let center_crop = parse_center_crop(center_crop)?;

    let save_original_file =
        if let Some(save_original_file) = multipart_form_data.texts.get("save_original_file") {
            let save_original_file = save_original_file.first().unwrap();

            match Boolean::parse_str(save_original_file.text.as_str()) {
                Ok(b) => b.0,
                Err(_) => return Err(Status::BadRequest),
            }
        } else {
            true
        };

    match datalith
        .put_image_by_path(
            file_field.path.as_path(),
            file_name.as_ref(),
            max_width,
            max_height,
            center_crop,
            save_original_file,
        )
        .await
    {
        Ok(file) => {
            let value = datalith_image_to_json_value(file);

            Ok(RawJson(serde_json::to_string(&value).unwrap()))
        },
        Err(error) => {
            rocket::error!("{error}");

            if let DatalithImageWriteError::UnsupportedImageType = error {
                Err(Status::BadRequest)
            } else {
                Err(Status::InternalServerError)
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
#[put("/?<file_name>&<max_width>&<max_height>&<center_crop>&<save_original_file>", data = "<data>")]
async fn stream_upload(
    server_config: &State<ServerConfig>,
    datalith: &State<DatalithManager>,
    file_length: Option<&FileLength>,
    file_name: Option<&str>,
    max_width: Option<u16>,
    max_height: Option<u16>,
    center_crop: Option<&str>,
    save_original_file: Option<Boolean>,
    data: Data<'_>,
) -> Result<RawJson<String>, Status> {
    let expected_reader_length = validate_content_length(server_config, file_length)?;
    let center_crop = parse_center_crop(center_crop)?;
    let save_original_file = save_original_file.map(|e| e.0).unwrap_or(true);

    // max_file_size plus 1 in order to distinguish the too large payload
    let stream = data.open((server_config.max_file_size + 1).into());

    match datalith
        .put_image_by_reader(
            stream,
            file_name,
            max_width,
            max_height,
            center_crop,
            save_original_file,
            Some(expected_reader_length),
        )
        .await
    {
        Ok(file) => {
            let value = datalith_image_to_json_value(file);

            Ok(RawJson(serde_json::to_string(&value).unwrap()))
        },
        Err(DatalithImageWriteError::DatalithWriteError(
            DatalithWriteError::FileLengthTooLarge {
                ..
            },
        )) => Err(Status::PayloadTooLarge),
        Err(error) => {
            rocket::error!("{error}");

            if let DatalithImageWriteError::UnsupportedImageType = error {
                Err(Status::BadRequest)
            } else {
                Err(Status::InternalServerError)
            }
        },
    }
}

#[delete("/<id>")]
async fn delete(datalith: &State<DatalithManager>, id: Uuid) -> Result<&'static str, Status> {
    match datalith.delete_image_by_id(id).await {
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
    rocket.mount("/i/o", routes![upload, stream_upload, delete])
}

#[inline]
fn datalith_image_to_json_value(image: DatalithImage) -> Value {
    json!(
        {
            "id": image.id().to_string(),
            "created_at": image.created_at().to_rfc3339(),
            "image_width": image.image_width(),
            "image_height": image.image_height(),
            "image_stem": image.image_stem(),
        }
    )
}

#[inline]
fn parse_center_crop(center_crop: Option<&str>) -> Result<Option<CenterCrop>, Status> {
    if let Some(center_crop) = center_crop {
        let mut split = center_crop.split(':');

        let mut read_next_f64 = || {
            if let Some(t) = split.next() {
                t.parse::<f64>().map_err(|_| Status::BadRequest)
            } else {
                Err(Status::BadRequest)
            }
        };

        let w = read_next_f64()?;
        let h = read_next_f64()?;

        if split.next().is_some() {
            return Err(Status::BadRequest);
        }

        Ok(CenterCrop::new(w, h))
    } else {
        Ok(None)
    }
}
