use std::{path::Path, time::Duration};

use datalith_core::{
    ContentRequest, Datalith, DatalithService, MediaKind, PATH_DB_FILE, PATH_FILE_DIRECTORY,
    ServiceConfig, Uuid, chrono::Utc,
};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tempfile::TempDir;
use tokio::{fs, io::AsyncReadExt};

const PNG: &[u8] = include_bytes!("data/image.png");
const WEBP: &[u8] = include_bytes!("data/migration-thumbnail.webp");
const STANDALONE: &[u8] = b"A standalone legacy file.";

async fn legacy_file(
    pool: &SqlitePool,
    directory: &Path,
    id: Uuid,
    bytes: &[u8],
    mime: &str,
    expires_at: Option<i64>,
    count: i64,
) {
    let hash = if expires_at.is_some() { vec![0x55; 32] } else { Sha256::digest(bytes).to_vec() };
    sqlx::query(
        "INSERT INTO files(id,hash,created_at,file_size,file_type,file_name,count,expired_at) \
         VALUES(?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(hash)
    .bind(1_700_000_000_000i64)
    .bind(bytes.len() as i64)
    .bind(mime)
    .bind("legacy-file")
    .bind(count)
    .bind(expires_at)
    .execute(pool)
    .await
    .unwrap();
    fs::write(directory.join(PATH_FILE_DIRECTORY).join(format!("{:x}", id.as_u128())), bytes)
        .await
        .unwrap();
}

#[tokio::test]
async fn migrate_legacy_ids_images_aliases_and_reference_counts() {
    let directory = TempDir::new().unwrap();
    fs::create_dir(directory.path().join(PATH_FILE_DIRECTORY)).await.unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(directory.path().join(PATH_DB_FILE))
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("data/schema-v1.sql")).execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE sys_db_information(key TEXT PRIMARY KEY NOT NULL, value TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sys_db_information(key,value) \
         VALUES('version','1'),('create_time','2023-11-14T22:13:20Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let original_id = Uuid::from_u128(1);
    let alias_id = Uuid::from_u128(2);
    let webp_id = Uuid::from_u128(3);
    let standalone_id = Uuid::from_u128(4);
    let resource_id = Uuid::from_u128(11);
    let image_id = Uuid::from_u128(12);
    let temporary_resource_id = Uuid::from_u128(13);
    let expiry = Utc::now().timestamp_millis() + 600_000;
    legacy_file(&pool, directory.path(), original_id, PNG, "image/png", None, 8).await;
    legacy_file(&pool, directory.path(), alias_id, PNG, "image/png", Some(expiry), 1).await;
    legacy_file(&pool, directory.path(), webp_id, WEBP, "image/webp", None, 1).await;
    legacy_file(&pool, directory.path(), standalone_id, STANDALONE, "text/plain", None, 5).await;
    for (id, expires_at) in [(resource_id, None), (temporary_resource_id, Some(expiry))] {
        sqlx::query(
            "INSERT INTO resources(id,created_at,file_type,file_name,file_id,expired_at) \
             VALUES(?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(1_700_000_000_000i64)
        .bind("image/png")
        .bind("photo.png")
        .bind(original_id)
        .bind(expires_at)
        .execute(&pool)
        .await
        .unwrap();
    }
    let width = u32::from_be_bytes(PNG[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(PNG[20..24].try_into().unwrap());
    sqlx::query(
        "INSERT INTO \
         images(id,created_at,image_stem,image_width,image_height,original_file_id,\
         has_alpha_channel) VALUES(?,?,?,?,?,?,1)",
    )
    .bind(image_id)
    .bind(1_700_000_000_000i64)
    .bind("photo")
    .bind(width)
    .bind(height)
    .bind(original_id)
    .execute(&pool)
    .await
    .unwrap();
    for (fallback, file_id) in [(false, webp_id), (true, original_id)] {
        sqlx::query(
            "INSERT INTO image_thumbnails(image_id,multiplier,fallback,file_id) VALUES(?,1,?,?)",
        )
        .bind(image_id)
        .bind(fallback)
        .bind(file_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;
    let pending_backup = directory.path().join(format!("{PATH_DB_FILE}.v1.bak.pending"));
    fs::write(&pending_backup, b"An interrupted backup.").await.unwrap();

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    assert!(fs::try_exists(directory.path().join(format!("{PATH_DB_FILE}.v1.bak"))).await.unwrap());
    assert!(!fs::try_exists(pending_backup).await.unwrap());
    let backup = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(directory.path().join(format!("{PATH_DB_FILE}.v1.bak")))
            .read_only(true),
    )
    .await
    .unwrap();
    let check: String = sqlx::query_scalar("PRAGMA quick_check").fetch_one(&backup).await.unwrap();
    assert_eq!("ok", check);
    let version: String =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='version'")
            .fetch_one(&backup)
            .await
            .unwrap();
    assert_eq!("1", version);
    let old_count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
        .bind(original_id)
        .fetch_one(&backup)
        .await
        .unwrap();
    assert_eq!(8, old_count);
    backup.close().await;
    assert_eq!("5", service.list_media(1, 100).await.unwrap().total);
    let resource = service.get_media(resource_id).await.unwrap().unwrap();
    assert_eq!(original_id, resource.original.unwrap().id);
    assert_eq!("photo.png", resource.file_name);
    assert_eq!(1_700_000_000_000i64, resource.created_at.timestamp_millis());
    let image = service.get_media(image_id).await.unwrap().unwrap();
    assert_eq!(MediaKind::Image, image.kind);
    assert_eq!(original_id, image.original.unwrap().id);
    assert_eq!(2, image.variants.len());
    assert!(image.variants.iter().all(|variant| variant.recipe.is_none()
        && variant.name == "default"
        && variant.width == width
        && variant.height == height));
    let temporary = service.get_media(temporary_resource_id).await.unwrap().unwrap();
    assert!(temporary.single_use);
    assert_eq!(expiry, temporary.expires_at.unwrap().timestamp_millis());

    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
        .bind(original_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(4, count);
    let count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
        .bind(standalone_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(1, count);
    let storage_id: Uuid = sqlx::query_scalar("SELECT storage_id FROM blob_files WHERE file_id=?")
        .bind(alias_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(original_id, storage_id);
    assert!(
        !fs::try_exists(
            directory.path().join(PATH_FILE_DIRECTORY).join(format!("{:x}", alias_id.as_u128()))
        )
        .await
        .unwrap()
    );
    pool.close().await;

    let mut alias = service.open_content(alias_id, ContentRequest::default(), false).await.unwrap();
    assert_eq!(hex::encode(Sha256::digest(PNG)), alias.metadata.sha256);
    let mut bytes = Vec::new();
    alias.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(PNG, bytes);
    drop(alias);
    assert!(service.delete_media(image_id).await.unwrap());
    assert!(service.delete_media(resource_id).await.unwrap());
    assert!(service.delete_media(standalone_id).await.unwrap());
    service.close().await.unwrap();
    drop(service);

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!("1", service.list_media(1, 100).await.unwrap().total);
    let mut content = service
        .open_content(temporary_resource_id, ContentRequest::default(), false)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(PNG, bytes);
    drop(content);
    service.close().await.unwrap();
    drop(service);

    let service = tokio::time::timeout(
        Duration::from_secs(10),
        DatalithService::new(
            Datalith::new(directory.path()).await.unwrap(),
            ServiceConfig::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!("0", service.list_media(1, 100).await.unwrap().total);
    assert!(
        fs::read_dir(directory.path().join(PATH_FILE_DIRECTORY))
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    );
    service.close().await.unwrap();
}
