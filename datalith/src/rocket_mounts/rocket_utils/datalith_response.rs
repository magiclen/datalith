use std::collections::HashMap;

use datalith_core::{
    Datalith, DatalithFile, DatalithReadError, ReadableDatalithFile, Uuid,
    chrono::{DateTime, Local},
    mime::Mime,
};
use rocket::{Request, Response, http::Status, response, response::Responder};
use rocket_etag_if_none_match::{EtagIfNoneMatch, entity_tag::EntityTag};

#[derive(Debug)]
pub struct ResponseData {
    pub etag:          EntityTag<'static>,
    pub file:          ReadableDatalithFile,
    pub download:      bool,
    pub uuid:          Uuid,
    pub date:          DateTime<Local>,
    pub file_name:     String,
    pub file_type:     Mime,
    pub extra_headers: HashMap<&'static str, String>,
    pub is_temporary:  bool,
}

#[derive(Debug)]
pub struct DatalithResponse {
    pub(super) data: Option<ResponseData>,
}

impl DatalithResponse {
    #[inline]
    pub const fn is_temporary(&self) -> bool {
        if let Some(data) = self.data.as_ref() { data.is_temporary } else { false }
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
                    let uuid = resource.id();
                    let date = resource.created_at();

                    let file_name = resource.file_name().clone();
                    let file_type = resource.file_type().clone();
                    let is_temporary = resource.is_temporary();

                    Ok(Some(Self {
                        data: Some(ResponseData {
                            etag,
                            file: DatalithFile::from(resource).into_readable().await?,
                            download,
                            uuid,
                            date,
                            file_name,
                            file_type,
                            extra_headers: HashMap::new(),
                            is_temporary,
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

            response.raw_header("x-uuid", data.uuid.to_string());
            response.raw_header("date", data.date.to_rfc2822());
            response.raw_header("content-type", data.file_type.to_string());

            for (name, value) in data.extra_headers {
                response.raw_header(name, value);
            }

            response.sized_body(data.file.file_size().try_into().ok(), data.file);
        } else {
            response.status(Status::NotModified);
        }

        response.ok()
    }
}
