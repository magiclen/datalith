use std::num::ParseIntError;

use rocket::{Request, http::Status, outcome::Outcome, request, request::FromRequest};

#[derive(Debug, Clone, Default)]
pub struct FileLength(u64);

impl FileLength {
    #[inline]
    pub const fn to_u64(&self) -> u64 {
        self.0
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for FileLength {
    type Error = ParseIntError;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let content_length: Option<&str> = request.headers().get("content-length").next(); // Only fetch the first one.

        if let Some(content_length) = content_length {
            match content_length.parse::<u64>() {
                Ok(content_length) => Outcome::Success(Self(content_length)),
                Err(error) => Outcome::Error((Status::BadRequest, error)),
            }
        } else {
            let file_length: Option<&str> = request.headers().get("x-file-length").next(); // Only fetch the first one.

            if let Some(file_length) = file_length {
                match file_length.parse::<u64>() {
                    Ok(file_length) => Outcome::Success(Self(file_length)),
                    Err(error) => Outcome::Error((Status::BadRequest, error)),
                }
            } else {
                Outcome::Forward(Status::NotFound)
            }
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for &'r FileLength {
    type Error = ParseIntError;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let content_length: Option<&str> = request.headers().get("content-length").next(); // Only fetch the first one.

        if let Some(content_length) = content_length {
            match content_length.parse::<u64>() {
                Ok(content_length) => {
                    Outcome::Success(request.local_cache(|| FileLength(content_length)))
                },
                Err(error) => Outcome::Error((Status::BadRequest, error)),
            }
        } else {
            let file_length: Option<&str> = request.headers().get("x-file-length").next(); // Only fetch the first one.

            if let Some(file_length) = file_length {
                match file_length.parse::<u64>() {
                    Ok(file_length) => {
                        Outcome::Success(request.local_cache(|| FileLength(file_length)))
                    },
                    Err(error) => Outcome::Error((Status::BadRequest, error)),
                }
            } else {
                Outcome::Forward(Status::NotFound)
            }
        }
    }
}
