mod fetch;
#[cfg(feature = "image-convert")]
mod fetch_image;
mod operate;
#[cfg(feature = "image-convert")]
mod operate_image;
mod rocket_utils;

use std::net::IpAddr;

use rocket::{Build, Config, Request, Rocket, http::Status};
use validators::prelude::*;

#[derive(Debug)]
struct ServerConfig {
    pub(crate) max_file_size: u64,
}

#[derive(Debug, Clone, Copy, Validator)]
#[validator(boolean)]
struct Boolean(pub(crate) bool);

#[catch(default)]
fn default_error_catcher(status: Status, _req: &Request) -> String {
    format!("{status}")
}

pub fn create(address: IpAddr, listen_port: u16, max_file_size: u64) -> Rocket<Build> {
    let figment = Config::figment()
        .merge(("ident", "Datalith"))
        .merge(("address", address))
        .merge(("port", listen_port));

    let rocket = rocket::custom(figment)
        .manage(ServerConfig {
            max_file_size,
        })
        .register("/", catchers![default_error_catcher]);

    #[cfg(feature = "image-convert")]
    let rocket = fetch_image::mounts(rocket);

    #[cfg(feature = "image-convert")]
    let rocket = operate_image::mounts(rocket);

    let rocket = fetch::mounts(rocket);

    operate::mounts(rocket)
}
