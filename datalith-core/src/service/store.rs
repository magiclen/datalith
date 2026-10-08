use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use chrono::Utc;
#[cfg(any(feature = "image-convert", feature = "av-convert"))]
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use tokio::fs;
#[cfg(any(feature = "image-convert", feature = "av-convert"))]
use tokio::io::AsyncReadExt;
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

    /// Get a media item that has not expired or been consumed.
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

    /// Read metadata with an optional single-use playback credential.
    pub async fn get_media_with_session(
        &self,
        id: Uuid,
        session: Option<&str>,
    ) -> Result<Option<Media>, ServiceError> {
        let Some(token) = session else {
            return self.get_media(id).await;
        };
        match self.authorize_media(id, Some(token)).await {
            Ok(media) => Ok(Some(media)),
            Err(ServiceError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// List media items, newest first.
    /// `page` starts from 1, and `per_page` must be from 1 to 100.
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

    /// Open a file of a media item.
    /// Single-use images and resources are consumed unless `head` is `true`.
    pub async fn open_content(
        &self,
        id: Uuid,
        request: ContentRequest,
        head: bool,
    ) -> Result<Content, ServiceError> {
        self.open_content_with_session(id, request, head, None).await
    }

    /// Open content with a credential for single-use audio or video.
    pub async fn open_content_with_session(
        &self,
        id: Uuid,
        request: ContentRequest,
        head: bool,
        session: Option<&str>,
    ) -> Result<Content, ServiceError> {
        let media = self.authorize_media(id, session).await?;
        let playback = matches!(media.kind, MediaKind::Audio | MediaKind::Video);
        let _write_gate = if media.single_use && !head && !playback {
            Some(self.0.writes.try_read().map_err(|_| ServiceError::Busy)?)
        } else {
            None
        };
        let metadata = if media.kind == MediaKind::Resource
            || request.variant.as_deref() == Some("original")
        {
            media.original.clone().ok_or(ServiceError::NotFound)?
        } else if media.kind == MediaKind::Audio {
            let audio = media.audio.as_ref().ok_or(ServiceError::NotFound)?;
            audio
                .variants
                .iter()
                .find(|variant| {
                    request.variant.as_deref().is_none_or(|requested| requested == variant.id)
                        && request.format.as_deref().is_none_or(|format| {
                            format == variant.codec || format == "m4a" && variant.codec == "aac"
                        })
                        && request.multiplier.is_none_or(|multiplier| multiplier == 1)
                })
                .and_then(|variant| variant.file.clone())
                .ok_or(ServiceError::NotFound)?
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
        if playback {
            self.authorize_media(id, session).await?;
        }
        if media.single_use && !head && !playback {
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
            repeatable: !media.single_use || playback,
            temporary: media.expires_at.is_some() || media.single_use,
            _file_guard: Some(guard),
        })
    }

    /// Remove a media item; its files are removed later when nothing else uses them.
    /// Return `false` when the media does not exist.
    pub async fn delete_media(&self, id: Uuid) -> Result<bool, ServiceError> {
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let _mutation = self.0.mutations.lock().await;
        self.delete_media_inner(id).await
    }

    async fn delete_media_inner(&self, id: Uuid) -> Result<bool, ServiceError> {
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let rows: Vec<(Uuid, i64, Uuid)> = sqlx::query_as(
            "SELECT m.file_id, COUNT(*), COALESCE(b.storage_id, m.file_id) FROM media_files m \
             LEFT JOIN blob_files b ON b.file_id = m.file_id WHERE m.media_id = ? GROUP BY \
             m.file_id, b.storage_id",
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
        let mut released = Vec::new();
        for (id, count, storage_id) in rows {
            if Datalith::release_file_references_in_transaction(&mut tx, id, count as u64).await? {
                released.push(storage_id);
            }
        }
        tx.commit().await?;
        self.0.released_files.lock().unwrap().extend(released);
        Ok(deleted)
    }

    pub(super) async fn collect_garbage(&self) -> Result<(), ServiceError> {
        // `UNION` lets each part use its own index; `OR` would scan the whole table.
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM media WHERE expires_at <= ? UNION SELECT m.id FROM media m WHERE \
             consumed_at IS NOT NULL AND NOT EXISTS(SELECT 1 FROM playback_sessions s WHERE \
             s.media_id=m.id AND s.expires_at>?)",
        )
        .bind(Utc::now().timestamp_millis())
        .bind(Utc::now().timestamp_millis())
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        for id in ids {
            // Take the gate and lock for each deletion, so that other requests and a waiting export can run between them.
            let Ok(_gate) = self.0.writes.try_read() else {
                break;
            };
            let _mutation = self.0.mutations.lock().await;
            let eligible = sqlx::query(
                "SELECT 1 FROM media m WHERE id=? AND (expires_at<=? OR (consumed_at IS NOT NULL \
                 AND NOT EXISTS(SELECT 1 FROM playback_sessions s WHERE s.media_id=m.id AND \
                 s.expires_at>?)))",
            )
            .bind(id)
            .bind(Utc::now().timestamp_millis())
            .bind(Utc::now().timestamp_millis())
            .fetch_optional(&self.0.datalith.0.db)
            .await?
            .is_some();
            if eligible {
                self.delete_media_inner(id).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn clear_untracked_files(&self) -> Result<bool, ServiceError> {
        let Ok(_gate) = self.0.writes.try_read() else {
            return Ok(false);
        };
        let _mutation = self.0.mutations.lock().await;
        self.0
            .datalith
            .clear_untracked_files()
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(true)
    }

    pub(super) async fn clear_released_files(&self) -> Result<(), ServiceError> {
        let ids: Vec<_> = self.0.released_files.lock().unwrap().iter().copied().collect();
        for id in ids {
            let Ok(_gate) = self.0.writes.try_read() else {
                break;
            };
            let _mutation = self.0.mutations.lock().await;
            if self
                .0
                .datalith
                .clear_untracked_storage_file(id)
                .await
                .map_err(|error| ServiceError::Internal(error.to_string()))?
            {
                self.0.released_files.lock().unwrap().remove(&id);
            }
        }
        Ok(())
    }

    #[cfg(any(feature = "image-convert", feature = "av-convert"))]
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
        // Sync here, outside the publish transaction, so publishing only needs one directory sync.
        file.sync_all().await?;
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
        inventory: Option<&mut super::HlsInventory>,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
    ) -> Result<(), ServiceError> {
        super::validate_assets(media, inventory.as_deref())?;
        let mut inventory = inventory;
        let mut linked = false;
        for file in super::files_mut(media, inventory.as_deref_mut()) {
            let source_id = file.id;
            linked |= self.register_file(tx, file, prepared, guards, source_id, false).await?;
        }
        if linked {
            self.sync_file_directory().await?;
        }
        insert_media(tx, media, inventory.as_deref()).await?;
        Ok(())
    }

    pub(super) async fn publish_import_media_tx(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        media: &mut Media,
        inventory: Option<&mut super::HlsInventory>,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
        ids: &mut HashMap<Uuid, Uuid>,
    ) -> Result<bool, ServiceError> {
        super::validate_assets(media, inventory.as_deref())?;
        let mut inventory = inventory;
        let mut linked = false;
        for file in super::files_mut(media, inventory.as_deref_mut()) {
            let source_id = file.id;
            if let Some(id) = ids.get(&source_id) {
                file.id = *id;
            }
            linked |= self.register_file(tx, file, prepared, guards, source_id, true).await?;
            ids.insert(source_id, file.id);
        }
        insert_media(tx, media, inventory.as_deref()).await?;
        Ok(linked)
    }

    // Prepared files are synced before publishing, so one directory sync makes all new links durable before the commit.
    pub(super) async fn sync_file_directory(&self) -> Result<(), ServiceError> {
        sync_directory(&self.0.datalith.get_environment().join(PATH_FILE_DIRECTORY)).await
    }

    // Return `true` when a new file was linked into the file directory.
    async fn register_file(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        metadata: &mut MediaFile,
        prepared: &HashMap<Uuid, PreparedFile>,
        guards: &mut Vec<OpenGuard>,
        source_id: Uuid,
        preserve_id: bool,
    ) -> Result<bool, ServiceError> {
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
            return Ok(false);
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
            return Ok(false);
        }
        let directory = self.0.datalith.get_environment().join(PATH_FILE_DIRECTORY);
        fs::create_dir_all(&directory).await?;
        loop {
            // A removed file ID can still name shared content or protect an active reader.
            let reserved = sqlx::query(
                "SELECT 1 FROM files WHERE id = ? UNION ALL SELECT 1 FROM blob_files WHERE \
                 file_id = ? UNION ALL SELECT 1 FROM blob_files WHERE storage_id = ? LIMIT 1",
            )
            .bind(metadata.id)
            .bind(metadata.id)
            .bind(metadata.id)
            .fetch_optional(&mut **tx)
            .await?
            .is_some();
            let destination = directory.join(format!("{:x}", metadata.id.as_u128()));
            if !reserved && !fs::try_exists(&destination).await? {
                break;
            }
            metadata.id = Uuid::new_v4();
        }
        guards.push(
            OpenGuard::try_new(self.0.datalith.clone(), metadata.id).ok_or(ServiceError::Busy)?,
        );
        let (storage_id, linked) = if let Some((_, storage_id)) = existing {
            (storage_id, false)
        } else {
            let source = prepared
                .get(&source_id)
                .ok_or_else(|| ServiceError::Invalid("missing file content".into()))?;
            let destination = directory.join(format!("{:x}", metadata.id.as_u128()));
            // This runs inside the write transaction, so link the staged file instead of copying it; staged files are never changed after staging.
            if let Err(error) = fs::hard_link(&source.path, &destination).await {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
                // The fallback must stay on the destination file system and must never replace an existing file.
                let temporary = tempfile::NamedTempFile::new_in(&directory)?;
                fs::copy(&source.path, temporary.path()).await?;
                fs::File::open(temporary.path()).await?.sync_all().await?;
                temporary.persist_noclobber(&destination).map_err(|error| error.error)?;
            }
            (metadata.id, true)
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
        Ok(linked)
    }

    /// Open the archive created by a finished export task.
    pub async fn open_artifact(&self, id: Uuid) -> Result<Content, ServiceError> {
        self.open_artifact_with_session(id, None).await
    }

    /// Open a TAR or MP4 artifact, with a playback credential when required.
    pub async fn open_artifact_with_session(
        &self,
        id: Uuid,
        session: Option<&str>,
    ) -> Result<Content, ServiceError> {
        let guard = self.0.artifacts.read().await;
        let task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if !matches!(task.kind.as_str(), "export" | "mp4_export")
            || task.status != super::TaskStatus::Succeeded
        {
            return Err(ServiceError::NotFound);
        }
        let mut single_use = false;
        let path = if task.kind == "mp4_export" {
            let row = sqlx::query(
                "SELECT media_id, session_hash FROM mp4_artifacts WHERE task_id=? AND expires_at>?",
            )
            .bind(id)
            .bind(Utc::now().timestamp_millis())
            .fetch_optional(&self.0.datalith.0.db)
            .await?
            .ok_or(ServiceError::NotFound)?;
            if let Some(expected) = row.try_get::<Option<String>, _>("session_hash")? {
                let hash = super::sessions::token_hash(session.ok_or(ServiceError::NotFound)?)?;
                if expected != hash {
                    return Err(ServiceError::NotFound);
                }
                self.authorize_media_hash(row.try_get("media_id")?, Some(&hash)).await?;
                single_use = true;
            }
            self.work_directory(id).join("export.mp4")
        } else {
            self.work_directory(id).join("export.tar")
        };
        let metadata: MediaFile = serde_json::from_value(
            task.result.and_then(|result| result.get("artifact").cloned()).ok_or_else(|| {
                ServiceError::Internal("export task has no artifact metadata".into())
            })?,
        )?;
        let file = fs::File::open(path).await?;
        // An open file stays readable after cleanup removes its path, so cleanup does not wait for the download.
        drop(guard);
        Ok(Content {
            file,
            metadata,
            created_at: task.updated_at,
            single_use,
            repeatable: true,
            temporary: true,
            _file_guard: None,
        })
    }
}

pub(super) async fn sync_directory(path: &Path) -> Result<(), ServiceError> {
    fs::File::open(path).await?.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sha2::Digest;
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::{ServiceConfig, Task, TaskStatus, UploadOptions};

    async fn finished(service: &DatalithService, id: Uuid) -> Task {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let task = service.get_task(id).await.unwrap().unwrap();
                if task.status.is_terminal() {
                    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn wait_until(expiry: chrono::DateTime<Utc>) {
        while Utc::now() <= expiry {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn read_remaining(content: &mut Content, first: &[u8], expected: &[u8]) {
        let mut bytes = first.to_vec();
        content.file.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(expected, bytes);
        assert_eq!(hex::encode(sha2::Sha256::digest(&bytes)), content.metadata.sha256);
    }

    #[tokio::test]
    async fn archive_cleanup_does_not_wait_for_its_open_reader() {
        let directory = tempfile::tempdir().unwrap();
        let service =
            DatalithService::new(Datalith::new(directory.path()).await.unwrap(), ServiceConfig {
                task_retention_seconds: 1,
                ..ServiceConfig::default()
            })
            .await
            .unwrap();
        let upload = service
            .submit_upload(b"archived content".as_slice(), Default::default(), None)
            .await
            .unwrap();
        finished(&service, upload.id).await;
        let task = service.submit_export(Default::default(), None).await.unwrap();
        let task = finished(&service, task.id).await;
        let path = service.work_directory(task.id).join("export.tar");
        let expected = fs::read(&path).await.unwrap();
        let mut content = service.open_artifact(task.id).await.unwrap();
        let mut first = [0; 64];
        content.file.read_exact(&mut first).await.unwrap();
        wait_until(task.updated_at + chrono::Duration::milliseconds(1001)).await;
        service.expire_tasks().await.unwrap();
        assert!(!fs::try_exists(path).await.unwrap());
        assert!(matches!(service.open_artifact(task.id).await, Err(ServiceError::NotFound)));
        read_remaining(&mut content, &first, &expected).await;
        drop(content);
        service.close().await.unwrap();
    }

    #[cfg(feature = "av-convert")]
    #[tokio::test]
    async fn mp4_cleanup_does_not_wait_for_its_open_reader() {
        use crate::{Mp4ExportOptions, Mp4ExportResult, VideoOptions, VideoVariantSpec};

        let directory = tempfile::tempdir().unwrap();
        let mut config = ServiceConfig {
            mp4_export_retention_seconds: 1,
            ..ServiceConfig::default()
        };
        if let Some(path) = std::env::var_os("DATALITH_FFMPEG") {
            config.av.ffmpeg = path.into();
        }
        if let Some(path) = std::env::var_os("DATALITH_FFPROBE") {
            config.av.ffprobe = path.into();
        }
        let ffmpeg = config.av.ffmpeg.clone();
        let service = DatalithService::new(
            Datalith::new(directory.path().join("store")).await.unwrap(),
            config,
        )
        .await
        .unwrap();
        if !service.0.av.video_available {
            service.close().await.unwrap();
            return;
        }
        let input = directory.path().join("source.mp4");
        let output = tokio::process::Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:size=256x144:rate=12:duration=1",
                "-c:v",
                "libx264",
                "-preset",
                "fast",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&input)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let upload = service
            .submit_upload_file(
                input,
                UploadOptions {
                    kind: MediaKind::Video,
                    video: VideoOptions {
                        variants: vec![VideoVariantSpec {
                            resolution: 144, fps: 12
                        }],
                        ..VideoOptions::default()
                    },
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let media: Media =
            serde_json::from_value(finished(&service, upload.id).await.result.unwrap()).unwrap();
        let task = service
            .submit_mp4_export(
                media.id,
                Mp4ExportOptions {
                    variant: "144p12".into()
                },
                None,
                None,
            )
            .await
            .unwrap();
        let result: Mp4ExportResult =
            serde_json::from_value(finished(&service, task.id).await.result.unwrap()).unwrap();
        let path = service.work_directory(task.id).join("export.mp4");
        let expected = fs::read(&path).await.unwrap();
        let mut content = service.open_artifact(task.id).await.unwrap();
        let mut first = [0; 64];
        content.file.read_exact(&mut first).await.unwrap();
        wait_until(result.expires_at).await;
        service.expire_mp4_artifacts().await.unwrap();
        assert!(!fs::try_exists(path).await.unwrap());
        assert!(matches!(service.open_artifact(task.id).await, Err(ServiceError::NotFound)));
        read_remaining(&mut content, &first, &expected).await;
        drop(content);
        service.close().await.unwrap();
    }

    #[tokio::test]
    async fn released_content_is_collected_after_its_reader_closes() {
        let directory = tempfile::tempdir().unwrap();
        let service = DatalithService::new(
            Datalith::new(directory.path()).await.unwrap(),
            ServiceConfig::default(),
        )
        .await
        .unwrap();
        let task = service
            .submit_upload(b"content".as_slice(), UploadOptions::default(), None)
            .await
            .unwrap();
        let media: Media = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let task = service.get_task(task.id).await.unwrap().unwrap();
                if task.status.is_terminal() {
                    assert_eq!(TaskStatus::Succeeded, task.status);
                    break serde_json::from_value(task.result.unwrap()).unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let content =
            service.open_content(media.id, ContentRequest::default(), false).await.unwrap();
        let path = service.0.datalith.get_file_path(content.metadata.id).await.unwrap();
        service.delete_media(media.id).await.unwrap();
        service.clear_released_files().await.unwrap();
        assert!(fs::try_exists(&path).await.unwrap());
        drop(content);
        service.clear_released_files().await.unwrap();
        assert!(!fs::try_exists(path).await.unwrap());
        service.close().await.unwrap();
    }

    #[tokio::test]
    async fn hls_inventory_references_share_content_and_are_released_together() {
        use sha2::{Digest, Sha256};

        use super::super::{
            HlsInventory, HlsSegment, HlsTrack, ProcessingMethod, Rational, VideoMedia,
            VideoVariant,
        };

        let directory = tempfile::tempdir().unwrap();
        let service = DatalithService::new(
            Datalith::new(directory.path()).await.unwrap(),
            ServiceConfig::default(),
        )
        .await
        .unwrap();
        let bytes = b"Shared HLS content for a storage lifecycle test.";
        let path = directory.path().join("staged-fragment");
        fs::write(&path, bytes).await.unwrap();
        let file = MediaFile {
            id:        Uuid::new_v4(),
            sha256:    hex::encode(Sha256::digest(bytes)),
            file_size: bytes.len().to_string(),
            file_type: "video/mp4".into(),
            file_name: "fragment.m4s".into(),
        };
        let mut prepared = HashMap::new();
        prepared.insert(file.id, PreparedFile {
            path,
            metadata: file.clone(),
        });
        let mut media = Media {
            id:          Uuid::new_v4(),
            kind:        MediaKind::Video,
            created_at:  Utc::now(),
            file_name:   "video".into(),
            original:    Some(file.clone()),
            variants:    Vec::new(),
            audio:       None,
            video:       Some(VideoMedia {
                duration_seconds: 12.0,
                variants:         vec![VideoVariant {
                    id:                   "1080p30".into(),
                    resolution:           1080,
                    width:                1920,
                    height:               1080,
                    fps:                  30,
                    frame_rate:           Rational {
                        numerator: 30, denominator: 1
                    },
                    leading_hold_seconds: 0.0,
                    codec:                "avc1.640028".into(),
                    processing_method:    ProcessingMethod::Remuxed,
                    playlist_path:        String::new(),
                    audio:                Vec::new(),
                }],
                audio:            Vec::new(),
                master_path:      String::new(),
            }),
            warnings:    Vec::new(),
            expires_at:  None,
            single_use:  false,
            consumed_at: None,
            animated:    false,
            frame_count: 1,
        };
        let mut inventory = HlsInventory {
            presentation_start: 0,
            duration:           1_440_000,
            timescale:          120_000,
            tracks:             vec![HlsTrack {
                id:                 "1080p30".into(),
                initialization:     file.clone(),
                segments:           (0..2)
                    .map(|index| HlsSegment {
                        file:        file.clone(),
                        start:       index * 720_000,
                        duration:    720_000,
                        independent: true,
                    })
                    .collect(),
                timescale:          120_000,
                codec:              "avc1.640028".into(),
                average_bandwidth:  1000,
                peak_bandwidth:     1000,
                presentation_start: 0,
                presentation_end:   1_440_000,
                skip_samples:       0,
                discard_padding:    0,
            }],
        };
        let mut guards = Vec::new();
        let mut tx = service.0.datalith.0.db.begin().await.unwrap();
        service
            .publish_media_tx(&mut tx, &mut media, Some(&mut inventory), &prepared, &mut guards)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(guards);
        let references = super::super::file_references(&media, Some(&inventory));
        assert_eq!(4, references.len());
        assert!(references.iter().all(|(_, reference)| reference.id == file.id));
        let count: i64 = sqlx::query_scalar("SELECT count FROM files WHERE id=?")
            .bind(file.id)
            .fetch_one(&service.0.datalith.0.db)
            .await
            .unwrap();
        assert_eq!(4, count);
        let metadata: String = sqlx::query_scalar("SELECT metadata FROM media WHERE id=?")
            .bind(media.id)
            .fetch_one(&service.0.datalith.0.db)
            .await
            .unwrap();
        assert!(!metadata.contains("segments"));
        let stored: String = sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
            .bind(media.id)
            .fetch_one(&service.0.datalith.0.db)
            .await
            .unwrap();
        assert_eq!(
            2,
            serde_json::from_str::<HlsInventory>(&stored).unwrap().tracks[0].segments.len()
        );
        let content_path = service.0.datalith.get_file_path(file.id).await.unwrap();
        assert!(service.delete_media(media.id).await.unwrap());
        service.clear_released_files().await.unwrap();
        assert!(!fs::try_exists(content_path).await.unwrap());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_hls WHERE media_id=?")
            .bind(media.id)
            .fetch_one(&service.0.datalith.0.db)
            .await
            .unwrap();
        assert_eq!(0, count);
        service.close().await.unwrap();
    }
}
