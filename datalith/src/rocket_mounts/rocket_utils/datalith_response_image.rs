use std::{collections::HashMap, fmt::Write, path::Path};

use datalith_core::{get_image_extension, mime, Datalith, DatalithReadError, Uuid, MIME_WEBP};
use rocket::{
    form,
    form::{FromFormField, ValueField},
};
use rocket_etag_if_none_match::{entity_tag::EntityTag, EtagIfNoneMatch};

use super::{DatalithResponse, ResponseData};

#[derive(Debug)]
pub enum ResolutionType {
    Original,
    Multiplier(u8),
}

#[rocket::async_trait]
impl<'r> FromFormField<'r> for ResolutionType {
    fn from_value(field: ValueField<'r>) -> form::Result<'r, Self> {
        if field.value.eq_ignore_ascii_case("original") {
            return Ok(Self::Original);
        }

        if let Some(v) = field.value.strip_suffix("x") {
            if let Ok(v) = v.parse::<u8>() {
                return Ok(Self::Multiplier(v));
            }
        }

        let mut errors = form::Errors::new();

        errors.push(form::Error::validation("not 1x, 2x, 3x, ..., etc"));

        Err(errors)
    }
}

impl DatalithResponse {
    pub async fn from_image_id<'a>(
        datalith: &'a Datalith,
        etag_if_none_match: &EtagIfNoneMatch<'a>,
        id: Uuid,
        resolution_type: Option<ResolutionType>,
        fallback: bool,
        download: bool,
    ) -> Result<Option<DatalithResponse>, DatalithReadError> {
        let etag = EntityTag::with_string(true, format!("{:x}", id.as_u128())).unwrap();

        let is_etag_match = etag_if_none_match.weak_eq(&etag);

        if is_etag_match {
            Ok(Some(DatalithResponse {
                data: None
            }))
        } else {
            let resolution_type = resolution_type.unwrap_or(ResolutionType::Multiplier(1));

            let image = datalith.get_image_by_id(id).await?;

            match image {
                Some(image) => {
                    let uuid = image.id();
                    let date = image.created_at();

                    let mut file_name = image.image_stem().clone();
                    let image_width = image.image_width();
                    let image_height = image.image_height();
                    let has_alpha_channel = image.has_alpha_channel();

                    let mut extra_headers = HashMap::with_capacity(2);

                    let (file, multiplier) = match resolution_type {
                        ResolutionType::Original => {
                            if image.original_file().is_some() {
                                (image.into_original_file().unwrap(), 0)
                            } else {
                                let v = if fallback {
                                    image.into_fallback_thumbnails()
                                } else {
                                    image.into_thumbnails()
                                };
                                let multiplier = v.len();

                                (v.into_iter().next_back().unwrap(), multiplier)
                            }
                        },
                        ResolutionType::Multiplier(multiplier) => {
                            let multiplier =
                                (multiplier as usize).clamp(1, image.thumbnails().len());
                            let v = if fallback {
                                image.into_fallback_thumbnails()
                            } else {
                                image.into_thumbnails()
                            };

                            (v.into_iter().nth(multiplier - 1).unwrap(), multiplier)
                        },
                    };

                    let file_type = if multiplier == 0 {
                        if let Some(ext) = get_image_extension(file.file_type()).or_else(|| {
                            Path::new(file.file_name()).extension().and_then(|e| e.to_str())
                        }) {
                            file_name.push('.');
                            file_name.push_str(ext);
                        }

                        file.file_type().clone()
                    } else {
                        let (ext, file_type) = if fallback {
                            if has_alpha_channel {
                                ("png", mime::IMAGE_PNG)
                            } else {
                                ("jpg", mime::IMAGE_JPEG)
                            }
                        } else {
                            ("webp", MIME_WEBP.clone())
                        };

                        file_name.write_fmt(format_args!("@{multiplier}x.{ext}")).unwrap();

                        let multiplier_u16 = multiplier as u16;

                        extra_headers
                            .insert("x-image-width", (image_width * multiplier_u16).to_string());
                        extra_headers
                            .insert("x-image-height", (image_height * multiplier_u16).to_string());

                        file_type
                    };

                    Ok(Some(Self {
                        data: Some(ResponseData {
                            etag,
                            file: file.into_readable().await?,
                            download,
                            uuid,
                            date,
                            file_name,
                            file_type,
                            extra_headers,
                            is_temporary: false,
                        }),
                    }))
                },
                None => Ok(None),
            }
        }
    }
}
