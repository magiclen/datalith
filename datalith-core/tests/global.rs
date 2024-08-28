#![allow(dead_code)]

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use datalith_core::{Datalith, DatalithCreateError};
use lazy_static_include::lazy_static_include_bytes;

const TEST_FOLDER: &str = slash_formatter::concat_with_file_separator!("tests", "db");

pub const IMAGE_PATH: &str = manifest_dir_macros::file_path!("tests", "data", "image.png");
pub const IMAGE_SIZE: u64 = 11658;

lazy_static_include_bytes! {
    pub IMAGE_DATA => ("tests", "data", "image.png"),
}

#[inline]
pub async fn datalith_init() -> Datalith {
    loop {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

        match Datalith::new(Path::new(TEST_FOLDER).join(timestamp.as_micros().to_string())).await {
            Ok(datalith) => break datalith,
            Err(DatalithCreateError::AlreadyRun) => continue,
            Err(error) => panic!("{error}"),
        }
    }
}

#[inline]
pub async fn datalith_close(datalith: Datalith) {
    datalith.drop_datalith().await.unwrap();
}
