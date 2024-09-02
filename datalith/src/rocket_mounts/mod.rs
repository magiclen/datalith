mod operate;

use std::net::IpAddr;

use rocket::{Build, Config, Rocket};

pub fn create(address: IpAddr, listen_port: u16) -> Rocket<Build> {
    let figment = Config::figment().merge(("address", address)).merge(("port", listen_port));

    let rocket = rocket::custom(figment);

    operate::mounts(rocket)
}
