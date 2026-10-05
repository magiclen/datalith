use datalith_core::{Datalith, DatalithCreateError, PATH_DB_FILE};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

#[tokio::test]
async fn fresh_store_creates_the_media_schema_without_legacy_tables() {
    let directory = tempfile::tempdir().unwrap();
    let datalith = Datalith::new(directory.path()).await.unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)).read_only(true),
    )
    .await
    .unwrap();
    let version: String =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='version'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!("2", version);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY \
         name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        [
            "archive_imports",
            "blob_files",
            "files",
            "media",
            "media_files",
            "media_hls",
            "mp4_artifacts",
            "playback_sessions",
            "sys_db_information",
            "tasks",
        ],
        tables.as_slice(),
    );
    let markers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sys_db_information WHERE key='media_migration'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(0, markers);
    pool.close().await;
    datalith.close().await;
}

#[tokio::test]
async fn the_store_lock_lasts_until_the_last_handle_is_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let datalith = Datalith::new(directory.path()).await.unwrap();
    let retained = datalith.clone();
    datalith.close().await;
    assert!(matches!(Datalith::new(directory.path()).await, Err(DatalithCreateError::AlreadyRun)));
    drop(retained);
    Datalith::new(directory.path()).await.unwrap().close().await;
}
