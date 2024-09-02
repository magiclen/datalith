use rocket::{Build, Rocket};

#[inline]
pub fn mounts(rocket: Rocket<Build>) -> Rocket<Build> {
    // rocket.mount("/o", routes![upload, stream_upload, delete])
    rocket
}
