use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use datalith_base::Datalith;

const TEST_FOLDER: &str = slash_formatter::concat_with_file_separator!("tests", "db");

#[inline]
pub async fn datalith_init() -> Datalith {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

    Datalith::new(Path::new(TEST_FOLDER).join(timestamp.as_micros().to_string())).await.unwrap()
}

#[inline]
pub async fn datalith_close(datalith: Datalith) {
    datalith.drop_database().await.unwrap();
}
