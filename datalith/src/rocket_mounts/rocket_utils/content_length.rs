use std::num::ParseIntError;

use rocket::{http::Status, outcome::Outcome, request, request::FromRequest, Request};

#[derive(Debug, Clone, Default)]
pub struct ContentLength(u64);

impl ContentLength {
    #[inline]
    pub const fn to_u64(&self) -> u64 {
        self.0
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ContentLength {
    type Error = ParseIntError;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let content_length: Option<&str> = request.headers().get("content-length").next(); // Only fetch the first one.

        if let Some(content_length) = content_length {
            match content_length.parse::<u64>() {
                Ok(content_length) => Outcome::Success(Self(content_length)),
                Err(error) => Outcome::Error((Status::NotFound, error)),
            }
        } else {
            Outcome::Forward(Status::NotFound)
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for &'r ContentLength {
    type Error = ParseIntError;

    async fn from_request(request: &'r Request<'_>) -> request::Outcome<Self, Self::Error> {
        let content_length: Option<&str> = request.headers().get("content-length").next(); // Only fetch the first one.

        if let Some(content_length) = content_length {
            match content_length.parse::<u64>() {
                Ok(content_length) => {
                    Outcome::Success(request.local_cache(|| ContentLength(content_length)))
                },
                Err(error) => Outcome::Error((Status::NotFound, error)),
            }
        } else {
            Outcome::Forward(Status::NotFound)
        }
    }
}
