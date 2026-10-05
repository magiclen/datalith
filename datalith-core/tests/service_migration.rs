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
const ORIGINAL_ID: Uuid = Uuid::from_u128(1);
const ALIAS_ID: Uuid = Uuid::from_u128(2);
const WEBP_ID: Uuid = Uuid::from_u128(3);
const STANDALONE_ID: Uuid = Uuid::from_u128(4);
const MISSING_ID: Uuid = Uuid::from_u128(5);
const RESOURCE_ID: Uuid = Uuid::from_u128(11);
const IMAGE_ID: Uuid = Uuid::from_u128(12);
const TEMPORARY_RESOURCE_ID: Uuid = Uuid::from_u128(13);

async fn legacy_file(
    pool: &SqlitePool,
    directory: &Path,
    id: Uuid,
    bytes: &[u8],
    mime: &str,
    expires_at: Option<i64>,
    count: i64,
) {
    let hash =
        if expires_at.is_some() { id.as_bytes().repeat(2) } else { Sha256::digest(bytes).to_vec() };
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

async fn legacy_store(directory: &Path) -> i64 {
    fs::create_dir(directory.join(PATH_FILE_DIRECTORY)).await.unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.join(PATH_DB_FILE)).create_if_missing(true),
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

    let expiry = Utc::now().timestamp_millis() + 600_000;
    legacy_file(&pool, directory, ORIGINAL_ID, PNG, "image/png", None, 8).await;
    legacy_file(&pool, directory, ALIAS_ID, PNG, "image/png", Some(expiry), 1).await;
    legacy_file(&pool, directory, WEBP_ID, WEBP, "image/webp", None, 1).await;
    legacy_file(&pool, directory, STANDALONE_ID, STANDALONE, "text/plain", None, 5).await;
    legacy_file(&pool, directory, MISSING_ID, STANDALONE, "text/plain", Some(expiry), 1).await;
    let missing_path =
        directory.join(PATH_FILE_DIRECTORY).join(format!("{:x}", MISSING_ID.as_u128()));
    fs::remove_file(missing_path).await.unwrap();
    for (id, expires_at) in [(RESOURCE_ID, None), (TEMPORARY_RESOURCE_ID, Some(expiry))] {
        sqlx::query(
            "INSERT INTO resources(id,created_at,file_type,file_name,file_id,expired_at) \
             VALUES(?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(1_700_000_000_000i64)
        .bind("image/png")
        .bind("photo.png")
        .bind(ORIGINAL_ID)
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
    .bind(IMAGE_ID)
    .bind(1_700_000_000_000i64)
    .bind("photo")
    .bind(width)
    .bind(height)
    .bind(ORIGINAL_ID)
    .execute(&pool)
    .await
    .unwrap();
    for (fallback, file_id) in [(false, WEBP_ID), (true, ORIGINAL_ID)] {
        sqlx::query(
            "INSERT INTO image_thumbnails(IMAGE_ID,multiplier,fallback,file_id) VALUES(?,1,?,?)",
        )
        .bind(IMAGE_ID)
        .bind(fallback)
        .bind(file_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;
    expiry
}

#[tokio::test]
async fn migrate_legacy_ids_images_aliases_and_reference_counts() {
    let directory = TempDir::new().unwrap();
    let expiry = legacy_store(directory.path()).await;
    let width = u32::from_be_bytes(PNG[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(PNG[20..24].try_into().unwrap());
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
        .bind(ORIGINAL_ID)
        .fetch_one(&backup)
        .await
        .unwrap();
    assert_eq!(8, old_count);
    backup.close().await;
    assert_eq!("5", service.list_media(1, 100).await.unwrap().total);
    let resource = service.get_media(RESOURCE_ID).await.unwrap().unwrap();
    assert_eq!(ORIGINAL_ID, resource.original.unwrap().id);
    assert_eq!("photo.png", resource.file_name);
    assert_eq!(1_700_000_000_000i64, resource.created_at.timestamp_millis());
    let image = service.get_media(IMAGE_ID).await.unwrap().unwrap();
    assert_eq!(MediaKind::Image, image.kind);
    assert_eq!(ORIGINAL_ID, image.original.unwrap().id);
    assert_eq!(2, image.variants.len());
    assert!(image.variants.iter().all(|variant| variant.recipe.is_none()
        && variant.name == "default"
        && variant.width == width
        && variant.height == height));
    let temporary = service.get_media(TEMPORARY_RESOURCE_ID).await.unwrap().unwrap();
    assert!(temporary.single_use);
    assert_eq!(expiry, temporary.expires_at.unwrap().timestamp_millis());

    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
        .bind(ORIGINAL_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(4, count);
    let count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
        .bind(STANDALONE_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(1, count);
    let storage_id: Uuid = sqlx::query_scalar("SELECT storage_id FROM blob_files WHERE file_id=?")
        .bind(ALIAS_ID)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ORIGINAL_ID, storage_id);
    assert!(
        !fs::try_exists(
            directory.path().join(PATH_FILE_DIRECTORY).join(format!("{:x}", ALIAS_ID.as_u128()))
        )
        .await
        .unwrap()
    );
    pool.close().await;

    assert_current_schema(directory.path()).await;
    assert_migrated_content(&service).await;
    let backup_path = directory.path().join(format!("{PATH_DB_FILE}.v1.bak"));
    let backup_bytes = fs::read(&backup_path).await.unwrap();
    service.close().await.unwrap();
    drop(service);

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    assert_current_schema(directory.path()).await;
    assert_migrated_content(&service).await;
    assert_eq!(backup_bytes, fs::read(&backup_path).await.unwrap());

    let mut alias = service.open_content(ALIAS_ID, ContentRequest::default(), false).await.unwrap();
    assert_eq!(hex::encode(Sha256::digest(PNG)), alias.metadata.sha256);
    let mut bytes = Vec::new();
    alias.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(PNG, bytes);
    drop(alias);
    assert!(service.delete_media(IMAGE_ID).await.unwrap());
    assert!(service.delete_media(RESOURCE_ID).await.unwrap());
    assert!(service.delete_media(STANDALONE_ID).await.unwrap());
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
        .open_content(TEMPORARY_RESOURCE_ID, ContentRequest::default(), false)
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

async fn assert_current_schema(directory: &Path) {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.join(PATH_DB_FILE)).read_only(true),
    )
    .await
    .unwrap();
    let version: String =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='version'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!("2", version);
    let legacy_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
         ('resources','images','image_thumbnails')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(0, legacy_tables);
    let markers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sys_db_information WHERE key='media_migration'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(0, markers);
    let violations = sqlx::query("PRAGMA foreign_key_check").fetch_all(&pool).await.unwrap();
    assert!(violations.is_empty());
    pool.close().await;
}

async fn assert_content(
    service: &DatalithService,
    id: Uuid,
    request: ContentRequest,
    expected: &[u8],
) {
    let mut content = service.open_content(id, request, true).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(expected, bytes);
    assert_eq!(hex::encode(Sha256::digest(expected)), content.metadata.sha256);
    assert_eq!(expected.len().to_string(), content.metadata.file_size);
}

async fn assert_migrated_content(service: &DatalithService) {
    for id in [RESOURCE_ID, ALIAS_ID, TEMPORARY_RESOURCE_ID] {
        assert_content(service, id, ContentRequest::default(), PNG).await;
    }
    assert_content(service, STANDALONE_ID, ContentRequest::default(), STANDALONE).await;
    let standalone = service.get_media(STANDALONE_ID).await.unwrap().unwrap();
    assert_eq!(STANDALONE_ID, standalone.original.unwrap().id);
    assert_eq!("legacy-file", standalone.file_name);
    assert_content(
        service,
        IMAGE_ID,
        ContentRequest {
            variant: Some("original".into()),
            ..ContentRequest::default()
        },
        PNG,
    )
    .await;
    let image = service.get_media(IMAGE_ID).await.unwrap().unwrap();
    for variant in image.variants {
        let expected = if variant.file.id == WEBP_ID { WEBP } else { PNG };
        assert_content(
            service,
            IMAGE_ID,
            ContentRequest {
                variant:    Some(variant.name),
                multiplier: Some(variant.multiplier),
                format:     Some(variant.format),
            },
            expected,
        )
        .await;
    }
}

#[tokio::test]
async fn failed_migration_restores_legacy_tables_and_retries_without_replacing_backup() {
    let directory = TempDir::new().unwrap();
    legacy_store(directory.path()).await;
    let file_directory = directory.path().join(PATH_FILE_DIRECTORY);
    let mut originals = std::collections::BTreeMap::new();
    let mut entries = fs::read_dir(&file_directory).await.unwrap();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        originals.insert(entry.file_name(), fs::read(entry.path()).await.unwrap());
    }
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)),
    )
    .await
    .unwrap();
    // Fail only after the legacy tables have been dropped.
    sqlx::raw_sql(
        "CREATE TRIGGER fail_migration BEFORE UPDATE OF value ON sys_db_information WHEN \
         NEW.key='version' AND NEW.value='2' AND NOT EXISTS (SELECT 1 FROM sqlite_master WHERE \
         name IN ('resources','images','image_thumbnails')) BEGIN SELECT RAISE(ABORT, 'migration \
         failed after DROP'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let error = Datalith::new(directory.path()).await.unwrap_err();
    assert!(error.to_string().contains("migration failed after DROP"));

    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let version: String =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key='version'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!("1", version);
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM resources), (SELECT COUNT(*) FROM images), (SELECT COUNT(*) \
         FROM image_thumbnails), (SELECT count FROM files WHERE id=?)",
    )
    .bind(ORIGINAL_ID)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((2, 1, 2, 8), counts);
    let media_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
         ('media','media_files','blob_files','tasks')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(0, media_tables);
    for (name, expected) in &originals {
        assert_eq!(*expected, fs::read(file_directory.join(name)).await.unwrap());
    }
    let backup_path = directory.path().join(format!("{PATH_DB_FILE}.v1.bak"));
    let backup_bytes = fs::read(&backup_path).await.unwrap();
    assert!(!backup_bytes.is_empty());
    sqlx::query("DROP TRIGGER fail_migration").execute(&pool).await.unwrap();
    pool.close().await;

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    assert_current_schema(directory.path()).await;
    assert_migrated_content(&service).await;
    assert_eq!(backup_bytes, fs::read(backup_path).await.unwrap());
    assert!(
        !fs::try_exists(directory.path().join(format!("{PATH_DB_FILE}.v1.bak.pending")))
            .await
            .unwrap()
    );
    service.close().await.unwrap();
}
