use datalith_core::{Datalith, DatalithFile, DatalithReadError, Uuid};
use rocket::{http::Status, response, response::Responder, Request, Response};
use rocket_etag_if_none_match::{entity_tag::EntityTag, EtagIfNoneMatch};

use super::response_data::ResponseData;

#[derive(Debug)]
pub struct DatalithResponse {
    pub(super) data: Option<ResponseData>,
}

impl DatalithResponse {
    #[inline]
    pub fn is_temporary(&self) -> bool {
        if let Some(data) = self.data.as_ref() {
            data.file.is_temporary()
        } else {
            false
        }
    }
}

impl DatalithResponse {
    pub async fn from_resource_id<'a>(
        datalith: &'a Datalith,
        etag_if_none_match: &EtagIfNoneMatch<'a>,
        id: Uuid,
        download: bool,
    ) -> Result<Option<DatalithResponse>, DatalithReadError> {
        let etag = EntityTag::with_string(true, format!("{:x}", id.as_u128())).unwrap();

        let is_etag_match = etag_if_none_match.weak_eq(&etag);

        if is_etag_match {
            Ok(Some(DatalithResponse {
                data: None
            }))
        } else {
            let resource = datalith.get_resource_by_id(id).await?;

            match resource {
                Some(resource) => {
                    let file_name = resource.file_name().clone();
                    let file_type = resource.file_type().clone();

                    Ok(Some(Self {
                        data: Some(ResponseData {
                            etag,
                            file: DatalithFile::from(resource).into_readable().await?,
                            download,
                            file_name,
                            file_type,
                        }),
                    }))
                },
                None => Ok(None),
            }
        }
    }
}

impl<'r, 'o: 'r> Responder<'r, 'o> for DatalithResponse {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'o> {
        let mut response = Response::build();

        if let Some(data) = self.data {
            if !data.file.is_temporary() {
                response.raw_header("etag", data.etag.to_string());
            }

            {
                let mut v = format!(
                    "{}; filename*=UTF-8''",
                    if data.download { "attachment" } else { "inline" }
                );

                url_escape::encode_component_to_string(data.file_name, &mut v);

                response.raw_header("content-disposition", v);
            }

            response.raw_header("content-type", data.file_type.to_string());

            response.raw_header("content-length", data.file.file_size().to_string());

            response.streamed_body(data.file);
        } else {
            response.status(Status::NotModified);
        }

        response.ok()
    }
}
