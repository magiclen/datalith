use datalith_core::DatalithManager;
use rocket::{http::Status, serde::uuid::Uuid, Build, Rocket, State};
use rocket_cache_response::CacheResponse;
use rocket_etag_if_none_match::EtagIfNoneMatch;

use crate::rocket_mounts::{
    rocket_utils::{DatalithResponse, ResolutionType},
    Boolean,
};

#[get("/<id>?<resolution>&<fallback>&<download>")]
async fn get(
    etag_if_none_match: &EtagIfNoneMatch<'_>,
    file_center: &State<DatalithManager>,
    id: Uuid,
    resolution: Option<ResolutionType>,
    fallback: Option<Boolean>,
    download: Option<Boolean>,
) -> Result<CacheResponse<DatalithResponse>, Status> {
    let fallback = fallback.map(|e| e.0).unwrap_or(false);
    let download = download.map(|e| e.0).unwrap_or(false);

    match DatalithResponse::from_image_id(
        file_center.inner(),
        etag_if_none_match,
        id,
        resolution,
        fallback,
        download,
    )
    .await
    {
        Ok(Some(response)) => {
            if response.is_temporary() {
                Ok(CacheResponse::NoStore(response))
            } else {
                Ok(CacheResponse::NoCacheControl(response))
            }
        },
        Ok(None) => Err(Status::NotFound),
        Err(error) => {
            rocket::error!("{error}");

            Err(Status::InternalServerError)
        },
    }
}

#[inline]
pub fn mounts(rocket: Rocket<Build>) -> Rocket<Build> {
    rocket.mount("/i/f", routes![get])
}
