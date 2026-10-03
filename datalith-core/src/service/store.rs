use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use tokio::{fs, io::AsyncReadExt};
use uuid::Uuid;

use super::{
    Content, ContentRequest, DatalithService, Media, MediaFile, MediaKind, Page, PreparedFile,
    ServiceError,
    migration::{insert_media, timestamp},
};
use crate::{Datalith, PATH_FILE_DIRECTORY, guard::OpenGuard};

impl DatalithService {
    pub(super) fn work_directory(&self, id: Uuid) -> PathBuf {
        self.0.datalith.get_environment().join("datalith.tasks").join(id.to_string())
    }

    pub async fn get_media(&self, id: Uuid) -> Result<Option<Media>, ServiceError> {
        let row = sqlx::query(
            "SELECT metadata, consumed_at FROM media WHERE id = ? AND (expires_at IS NULL OR \
             expires_at > ?) AND consumed_at IS NULL",
        )
        .bind(id)
        .bind(Utc::now().timestamp_millis())
        .fetch_optional(&self.0.datalith.0.db)
        .await?;
        row.map(|row| {
            let mut media: Media = serde_json::from_str(row.try_get("metadata")?)?;
            media.consumed_at =
                row.try_get::<Option<i64>, _>("consumed_at")?.map(timestamp).transpose()?;
            Ok(media)
        })
        .transpose()
    }

    pub async fn list_media(&self, page: u64, per_page: u64) -> Result<Page<Media>, ServiceError> {
        if page == 0 || !(1..=100).contains(&per_page) {
            return Err(ServiceError::Invalid(
                "page must be positive and per_page must be between 1 and 100".into(),
            ));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|v| v.checked_mul(per_page))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(|| ServiceError::Invalid("page is too large".into()))?;
        let now = Utc::now().timestamp_millis();
        let mut tx = self.0.datalith.0.db.begin().await?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media WHERE (expires_at IS NULL OR expires_at > ?) AND \
             consumed_at IS NULL",
        )
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT metadata FROM media WHERE (expires_at IS NULL OR expires_at > ?) AND \
             consumed_at IS NULL ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(now)
        .bind(per_page as i64)
        .bind(offset)
        .fetch_all(&mut *tx)
        .await?;
        let items = rows
            .into_iter()
            .map(|row| serde_json::from_str(&row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            items,
            page,
            per_page,
            total: total.to_string(),
        })
    }

    pub async fn open_content(
        &self,
        id: Uuid,
        request: ContentRequest,
        head: bool,
    ) -> Result<Content, ServiceError> {
        let media = self.get_media(id).await?.ok_or(ServiceError::NotFound)?;
        let _write_gate = if media.single_use && !head {
            Some(self.0.writes.try_read().map_err(|_| ServiceError::Busy)?)
        } else {
            None
        };
        let metadata = if media.kind == MediaKind::Resource
            || request.variant.as_deref() == Some("original")
        {
            media.original.clone().ok_or(ServiceError::NotFound)?
        } else {
            let name = request
                .variant
                .as_deref()
                .or_else(|| media.variants.first().map(|v| v.name.as_str()))
                .ok_or(ServiceError::NotFound)?;
            let format = request.format.as_deref().unwrap_or("webp");
            let multiplier = request.multiplier.unwrap_or(1);
            media
                .variants
                .iter()
                .find(|v| v.name == name && v.format == format && v.multiplier == multiplier)
                .ok_or(ServiceError::NotFound)?
                .file
                .clone()
        };
        let guard = OpenGuard::new(self.0.datalith.clone(), metadata.id).await;
        let path = self.0.datalith.get_file_path(metadata.id).await?;
        let file = match fs::File::open(path).await {
            Ok(file) => file,
            // The media can be deleted and its file removed after it was read above.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(if self.get_media(id).await?.is_some() {
                    ServiceError::Internal("the stored file is missing".into())
                } else {
                    ServiceError::NotFound
                });
            },
            Err(error) => return Err(error.into()),
        };
        if media.single_use && !head {
            let result = sqlx::query(
                "UPDATE media SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL AND \
                 (expires_at IS NULL OR expires_at > ?)",
            )
            .bind(Utc::now().timestamp_millis())
            .bind(id)
            .bind(Utc::now().timestamp_millis())
            .execute(&self.0.datalith.0.db)
            .await?;
            if result.rows_affected() != 1 {
                return Err(ServiceError::NotFound);
            }
        }
        Ok(Content {
            file,
            metadata,
            created_at: media.created_at,
            single_use: media.single_use,
            temporary: media.expires_at.is_some() || media.single_use,
            _file_guard: Some(guard),
            _artifact_guard: None,
        })
    }

    pub async fn delete_media(&self, id: Uuid) -> Result<bool, ServiceError> {
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let _mutation = self.0.mutations.lock().await;
        let result = self.delete_media_inner(id).await?;
        self.0.wakeup.notify_one();
        Ok(result)
    }

    async fn delete_media_inner(&self, id: Uuid) -> Result<bool, ServiceError> {
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT file_id, COUNT(*) FROM media_files WHERE media_id = ? GROUP BY file_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let deleted = sqlx::query("DELETE FROM media WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        let mut released = false;
        for (id, count) in rows {
            released |=
                Datalith::release_file_references_in_transaction(&mut tx, id, count as u64).await?;
        }
        tx.commit().await?;
        if released {
            self.0.released_files.store(true, Ordering::Release);
        }
        Ok(deleted)
    }

    pub(super) async fn collect_garbage(&self) -> Result<(), ServiceError> {
        let Ok(_gate) = self.0.writes.try_read() else {
            return Ok(());
        };
        // `UNION` lets each part use its own index; `OR` would scan the whole table.
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM media WHERE expires_at <= ? UNION SELECT id FROM media WHERE \
             consumed_at IS NOT NULL",
        )
        .bind(Utc::now().timestamp_millis())
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        for id in ids {
            // Lock for each deletion so that other requests can run between them.
            let _mutation = self.0.mutations.lock().await;
            self.delete_media_inner(id).await?;
        }
        Ok(())
    }

    pub(super) async fn clear_untracked_files(&self) -> Result<(), ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        self.0
            .datalith
            .clear_untracked_files()
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(())
    }

    pub(super) async fn prepare_file(
        path: PathBuf,
        file_type: String,
        file_name: String,
    ) -> Result<PreparedFile, ServiceError> {
        let mut file = fs::File::open(&path).await?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            size = size.checked_add(n as u64).ok_or(ServiceError::PayloadTooLarge)?;
            hasher.update(&buffer[..n]);
        }
        Ok(PreparedFile {
            path,
            metadata: MediaFile {
                id: Uuid::new_v4(),
                sha256: hex::encode(hasher.finalize()),
                file_size: size.to_string(),
                file_type,
                file_name,
            },
        })
    }

    pub(super) async fn publish_media_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        media: &mut Media,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
    ) -> Result<(), ServiceError> {
        if let Some(file) = &mut media.original {
            self.register_file(tx, file, prepared, guards, file.id, false).await?;
        }
        for variant in &mut media.variants {
            let source_id = variant.file.id;
            self.register_file(tx, &mut variant.file, prepared, guards, source_id, false).await?;
        }
        insert_media(tx, media).await?;
        Ok(())
    }

    pub(super) async fn publish_import_media_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        media: &mut Media,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
        ids: &mut HashMap<Uuid, Uuid>,
    ) -> Result<(), ServiceError> {
        for file in media
            .original
            .iter_mut()
            .chain(media.variants.iter_mut().map(|variant| &mut variant.file))
        {
            let source_id = file.id;
            if let Some(id) = ids.get(&source_id) {
                file.id = *id;
            }
            self.register_file(tx, file, prepared, guards, source_id, true).await?;
            ids.insert(source_id, file.id);
        }
        insert_media(tx, media).await?;
        Ok(())
    }

    async fn register_file(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        metadata: &mut MediaFile,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
        source_id: Uuid,
        preserve_id: bool,
    ) -> Result<(), ServiceError> {
        let hash = hex::decode(&metadata.sha256)
            .map_err(|_| ServiceError::Invalid("invalid SHA-256".into()))?;
        let taken: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT COALESCE(b.hash, f.hash) FROM files f LEFT JOIN blob_files b ON \
             b.file_id=f.id WHERE f.id=?",
        )
        .bind(metadata.id)
        .fetch_optional(&mut **tx)
        .await?;
        if preserve_id && taken.as_ref() == Some(&hash) {
            guards.push(
                OpenGuard::try_new(self.0.datalith.clone(), metadata.id)
                    .ok_or(ServiceError::Busy)?,
            );
            sqlx::query("UPDATE files SET count=count+1 WHERE id=?")
                .bind(metadata.id)
                .execute(&mut **tx)
                .await?;
            return Ok(());
        }
        if taken.is_some() {
            metadata.id = Uuid::new_v4();
        }
        // Legacy files keep the hash in `files`, while aliased files keep it in `blob_files`; each lookup uses its own index.
        let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, storage_id FROM (SELECT b.file_id AS id, b.storage_id AS storage_id FROM \
             blob_files b JOIN files f ON f.id = b.file_id WHERE b.hash = ? AND f.expired_at IS \
             NULL UNION ALL SELECT f.id, COALESCE(b.storage_id, f.id) FROM files f LEFT JOIN \
             blob_files b ON b.file_id = f.id WHERE f.hash = ? AND f.expired_at IS NULL) ORDER BY \
             id LIMIT 1",
        )
        .bind(&hash)
        .bind(&hash)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some((id, _)) = existing
            && !preserve_id
        {
            metadata.id = id;
            guards.push(OpenGuard::try_new(self.0.datalith.clone(), id).ok_or(ServiceError::Busy)?);
            sqlx::query("UPDATE files SET count=count+1 WHERE id=?")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            return Ok(());
        }
        guards.push(
            OpenGuard::try_new(self.0.datalith.clone(), metadata.id).ok_or(ServiceError::Busy)?,
        );
        let storage_id =
            if let Some((_, storage_id)) = existing {
                storage_id
            } else {
                let source = prepared
                    .get(&source_id)
                    .ok_or_else(|| ServiceError::Invalid("missing file content".into()))?;
                let directory = self.0.datalith.get_environment().join(PATH_FILE_DIRECTORY);
                fs::create_dir_all(&directory).await?;
                let destination = directory.join(format!("{:x}", metadata.id.as_u128()));
                // This runs inside the write transaction, so link the staged file instead of copying it; staged files are never changed after staging.
                if fs::hard_link(&source.path, &destination).await.is_err() {
                    let temporary =
                        tempfile::NamedTempFile::new_in(source.path.parent().ok_or_else(
                            || ServiceError::Invalid("invalid staging path".into()),
                        )?)?;
                    fs::copy(&source.path, temporary.path()).await?;
                    fs::rename(temporary.path(), &destination).await?;
                }
                // Image outputs are not synced when they are written.
                fs::File::open(&destination).await?.sync_all().await?;
                sync_directory(&directory).await?;
                metadata.id
            };
        let stored_hash = if storage_id == metadata.id {
            hash.clone()
        } else {
            crate::functions::get_random_hash().to_vec()
        };
        let size: i64 = metadata
            .file_size
            .parse()
            .map_err(|_| ServiceError::Invalid("file size is out of range".into()))?;
        sqlx::query(
            "INSERT INTO files(id,hash,created_at,file_size,file_type,file_name,count) \
             VALUES(?,?,?,?,?,?,1)",
        )
        .bind(metadata.id)
        .bind(stored_hash)
        .bind(Utc::now().timestamp_millis())
        .bind(size)
        .bind(&metadata.file_type)
        .bind(&metadata.file_name)
        .execute(&mut **tx)
        .await?;
        sqlx::query("INSERT INTO blob_files(file_id,hash,storage_id) VALUES(?,?,?)")
            .bind(metadata.id)
            .bind(hash)
            .bind(storage_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub async fn open_artifact(&self, id: Uuid) -> Result<Content, ServiceError> {
        let guard = self.0.artifacts.clone().read_owned().await;
        let task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if task.kind != "export" || task.status != super::TaskStatus::Succeeded {
            return Err(ServiceError::NotFound);
        }
        let path = self.work_directory(id).join("export.tar");
        let metadata: MediaFile = serde_json::from_value(
            task.result.and_then(|result| result.get("artifact").cloned()).ok_or_else(|| {
                ServiceError::Internal("export task has no artifact metadata".into())
            })?,
        )?;
        Ok(Content {
            file: fs::File::open(path).await?,
            metadata,
            created_at: task.updated_at,
            single_use: false,
            temporary: true,
            _file_guard: None,
            _artifact_guard: Some(guard),
        })
    }
}

pub(super) async fn sync_directory(path: &Path) -> Result<(), ServiceError> {
    #[cfg(unix)]
    fs::File::open(path).await?.sync_all().await?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
