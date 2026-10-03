use std::{collections::HashMap, io, path::Path};

use chrono::{DateTime, Utc};
use sqlx::{Pool, Row, Sqlite};
use uuid::Uuid;

use super::{Media, MediaFile, MediaKind, ServiceError, Variant};
use crate::{DatalithCreateError, PATH_DB_FILE, PATH_FILE_DIRECTORY, functions::get_hash_by_path};

pub(crate) async fn upgrade(
    pool: &Pool<Sqlite>,
    environment: &Path,
) -> Result<(), DatalithCreateError> {
    upgrade_inner(pool, environment).await.map_err(|error| match error {
        ServiceError::Database(error) => DatalithCreateError::SQLError(error),
        ServiceError::Io(error) => DatalithCreateError::IOError(error),
        error => DatalithCreateError::IOError(io::Error::new(
            io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    })
}

async fn upgrade_inner(pool: &Pool<Sqlite>, environment: &Path) -> Result<(), ServiceError> {
    let version: String =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key = 'version'")
            .fetch_one(pool)
            .await?;
    let ready: Option<String> =
        sqlx::query_scalar("SELECT value FROM sys_db_information WHERE key = 'media_migration'")
            .fetch_optional(pool)
            .await?;
    if ready.as_deref() == Some("2") {
        // Add new indexes to databases created by earlier versions.
        sqlx::raw_sql(include_str!("../sql/service.sql")).execute(pool).await?;

        return Ok(());
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files").fetch_one(pool).await?;
    if version == "1" && count > 0 {
        let backup = environment.join(format!("{PATH_DB_FILE}.v1.bak"));
        if !tokio::fs::try_exists(&backup).await? {
            let pending = environment.join(format!("{PATH_DB_FILE}.v1.bak.pending"));
            match tokio::fs::remove_file(&pending).await {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            sqlx::query("VACUUM INTO ?")
                .bind(pending.to_string_lossy().as_ref())
                .execute(pool)
                .await?;
            tokio::fs::File::open(&pending).await?.sync_all().await?;
            tokio::fs::rename(pending, &backup).await?;
            super::store::sync_directory(environment).await?;
        }
    }
    let rows = sqlx::query(
        "SELECT id, hash, created_at, file_size, file_type, file_name, expired_at FROM files \
         ORDER BY expired_at IS NOT NULL, id",
    )
    .fetch_all(pool)
    .await?;
    let now = Utc::now().timestamp_millis();
    let mut files = HashMap::new();
    let mut storage = HashMap::<Vec<u8>, Uuid>::new();
    let mut mappings = Vec::new();
    let mut dropped = Vec::new();
    for row in &rows {
        let id: Uuid = row.try_get("id")?;
        let expired: Option<i64> = row.try_get("expired_at")?;
        let hash: Vec<u8> = match expired {
            // Temporary files are disposable, so drop the expired ones and the ones whose content is gone.
            Some(expired_at) if expired_at <= now => {
                dropped.push(id);
                continue;
            },
            Some(_) => match get_hash_by_path(
                environment.join(PATH_FILE_DIRECTORY).join(format!("{:x}", id.as_u128())),
            )
            .await
            {
                Ok(hash) => hash.to_vec(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    dropped.push(id);
                    continue;
                },
                Err(error) => return Err(error.into()),
            },
            None => row.try_get("hash")?,
        };
        let storage_id = *storage.entry(hash.clone()).or_insert(id);
        files.insert(id, MediaFile {
            id,
            sha256: hex::encode(&hash),
            file_size: row.try_get::<i64, _>("file_size")?.to_string(),
            file_type: row.try_get("file_type")?,
            file_name: row.try_get("file_name")?,
        });
        mappings.push((id, hash, storage_id));
    }
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(include_str!("../sql/service.sql")).execute(&mut *tx).await?;
    for (id, hash, storage_id) in mappings {
        sqlx::query("INSERT INTO blob_files(file_id, hash, storage_id) VALUES(?, ?, ?)")
            .bind(id)
            .bind(hash)
            .bind(storage_id)
            .execute(&mut *tx)
            .await?;
    }
    let resources = sqlx::query(
        "SELECT id, created_at, file_name, file_type, file_id, expired_at FROM resources",
    )
    .fetch_all(&mut *tx)
    .await?;
    let images = sqlx::query(
        "SELECT id, created_at, image_stem, image_width, image_height, original_file_id FROM \
         images",
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in resources {
        let id: Uuid = row.try_get("id")?;
        let file_id: Uuid = row.try_get("file_id")?;
        let mut file = files
            .get(&file_id)
            .ok_or_else(|| ServiceError::Invalid("legacy resource has a missing file".into()))?
            .clone();
        file.file_name = row.try_get("file_name")?;
        file.file_type = row.try_get("file_type")?;
        let expires_at: Option<i64> = row.try_get("expired_at")?;
        let media = Media {
            id,
            kind: MediaKind::Resource,
            created_at: timestamp(row.try_get("created_at")?)?,
            file_name: file.file_name.clone(),
            original: Some(file),
            variants: Vec::new(),
            expires_at: expires_at.map(timestamp).transpose()?,
            single_use: expires_at.is_some(),
            consumed_at: None,
            animated: false,
            frame_count: 1,
        };
        insert_media(&mut tx, &media).await?;
    }
    for row in images {
        let id: Uuid = row.try_get("id")?;
        let original_id: Option<Uuid> = row.try_get("original_file_id")?;
        let width: u32 = row.try_get("image_width")?;
        let height: u32 = row.try_get("image_height")?;
        let mut media = Media {
            id,
            kind: MediaKind::Image,
            created_at: timestamp(row.try_get("created_at")?)?,
            file_name: row.try_get("image_stem")?,
            original: original_id
                .map(|id| {
                    files.get(&id).cloned().ok_or_else(|| {
                        ServiceError::Invalid("legacy image has a missing original".into())
                    })
                })
                .transpose()?,
            variants: Vec::new(),
            expires_at: None,
            single_use: false,
            consumed_at: None,
            animated: false,
            frame_count: 1,
        };
        let thumbnails = sqlx::query(
            "SELECT multiplier, file_id FROM image_thumbnails WHERE image_id = ? ORDER BY \
             multiplier, fallback",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for row in thumbnails {
            let multiplier: u8 = row.try_get("multiplier")?;
            let file_id: Uuid = row.try_get("file_id")?;
            let file = files
                .get(&file_id)
                .ok_or_else(|| ServiceError::Invalid("legacy image has a missing variant".into()))?
                .clone();
            let format = file.file_type.strip_prefix("image/").unwrap_or("bin").to_owned();
            media.variants.push(Variant {
                name: "default".into(),
                multiplier,
                width: width * u32::from(multiplier),
                height: height * u32::from(multiplier),
                animated: false,
                content_path: content_path(id, "default", multiplier, &format),
                format,
                file,
                recipe: None,
            });
        }
        insert_media(&mut tx, &media).await?;
    }
    // Keep standalone files available under their original IDs.
    for row in &rows {
        let id: Uuid = row.try_get("id")?;
        let Some(file) = files.get(&id).cloned() else {
            continue;
        };
        let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_files WHERE file_id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if used != 0 {
            continue;
        }
        let expires_at: Option<i64> = row.try_get("expired_at")?;
        let media = Media {
            id,
            kind: MediaKind::Resource,
            created_at: timestamp(row.try_get("created_at")?)?,
            file_name: file.file_name.clone(),
            original: Some(file),
            variants: Vec::new(),
            expires_at: expires_at.map(timestamp).transpose()?,
            single_use: expires_at.is_some(),
            consumed_at: None,
            animated: false,
            frame_count: 1,
        };
        insert_media(&mut tx, &media).await?;
    }
    sqlx::query("DELETE FROM image_thumbnails").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM images").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM resources").execute(&mut *tx).await?;
    for id in dropped {
        sqlx::query("DELETE FROM files WHERE id = ?").bind(id).execute(&mut *tx).await?;
    }
    sqlx::query(
        "UPDATE files SET expired_at = NULL, count = (SELECT COUNT(*) FROM media_files WHERE \
         file_id = files.id)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE sys_db_information SET value = '2' WHERE key = 'version'")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT OR REPLACE INTO sys_db_information(key,value) VALUES('media_migration','2')",
    )
    .execute(&mut *tx)
    .await?;
    let violations = sqlx::query("PRAGMA foreign_key_check").fetch_all(&mut *tx).await?;
    if !violations.is_empty() {
        return Err(ServiceError::Invalid("legacy database has invalid references".into()));
    }
    tx.commit().await?;
    Ok(())
}

pub(super) fn timestamp(value: i64) -> Result<DateTime<Utc>, ServiceError> {
    DateTime::from_timestamp_millis(value)
        .ok_or_else(|| ServiceError::Invalid("invalid timestamp".into()))
}

pub(super) fn content_path(id: Uuid, name: &str, multiplier: u8, format: &str) -> String {
    format!("api/v1/media/{id}/content?variant={name}&multiplier={multiplier}&format={format}")
}

pub(super) async fn insert_media(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    media: &Media,
) -> Result<(), ServiceError> {
    sqlx::query(
        "INSERT INTO media(id, kind, created_at, expires_at, single_use, consumed_at, metadata) \
         VALUES(?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(media.id)
    .bind(match media.kind {
        MediaKind::Resource => "resource",
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    })
    .bind(media.created_at.timestamp_millis())
    .bind(media.expires_at.map(|v| v.timestamp_millis()))
    .bind(media.single_use)
    .bind(media.consumed_at.map(|v| v.timestamp_millis()))
    .bind(serde_json::to_string(media)?)
    .execute(&mut **tx)
    .await?;
    if let Some(file) = &media.original {
        insert_reference(tx, media.id, "original", file.id).await?;
    }
    for variant in &media.variants {
        insert_reference(
            tx,
            media.id,
            &format!("{}:{}:{}", variant.name, variant.multiplier, variant.format),
            variant.file.id,
        )
        .await?;
    }
    Ok(())
}

async fn insert_reference(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    id: Uuid,
    role: &str,
    file: Uuid,
) -> Result<(), ServiceError> {
    sqlx::query("INSERT INTO media_files(media_id, role, file_id) VALUES(?, ?, ?)")
        .bind(id)
        .bind(role)
        .bind(file)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
