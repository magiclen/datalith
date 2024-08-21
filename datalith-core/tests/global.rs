#![allow(dead_code)]

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use datalith_core::Datalith;

const TEST_FOLDER: &str = slash_formatter::concat_with_file_separator!("tests", "db");

pub const IMAGE_PATH: &str = manifest_dir_macros::file_path!("tests", "data", "image.png");
pub const IMAGE_SIZE: u64 = 11658;

#[inline]
pub async fn datalith_init() -> Datalith {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

    Datalith::new(Path::new(TEST_FOLDER).join(timestamp.as_micros().to_string())).await.unwrap()
}

#[inline]
pub async fn datalith_close(datalith: Datalith) {
    datalith.drop_datalith().await.unwrap();
}
