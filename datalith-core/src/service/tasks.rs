use std::{
    collections::HashMap,
    path::PathBuf,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, Transaction};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

use super::{
    DatalithService, ExportOptions, Media, MediaKind, ProcessOptions, ServiceError, Task,
    TaskError, TaskStatus, UploadOptions, Work, store::sync_directory,
};
#[cfg(feature = "image-convert")]
use super::{Variant, migration::content_path};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(60 * 60);

struct PendingDirectory {
    path:      PathBuf,
    committed: bool,
}

impl Drop for PendingDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl DatalithService {
    /// Queue an upload; the returned task creates the media.
    /// A repeated request with the same idempotency key and content returns the first task.
    pub async fn submit_upload(
        &self,
        reader: impl AsyncRead + Unpin,
        options: UploadOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        self.validate_upload(&options)?;
        let (id, directory, hash) = self.stage_input(reader).await?;
        self.enqueue(
            id,
            Work::Upload {
                options,
                hash,
            },
            idempotency_key,
            directory,
        )
        .await
    }

    /// Queue the import of a Datalith archive.
    pub async fn submit_import(
        &self,
        reader: impl AsyncRead + Unpin,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        let (id, directory, hash) = self.stage_input(reader).await?;
        self.enqueue(
            id,
            Work::Import {
                hash,
            },
            idempotency_key,
            directory,
        )
        .await
    }

    /// Queue an export of media into a Datalith archive.
    /// All media are exported when `options.ids` is `None`.
    pub async fn submit_export(
        &self,
        options: ExportOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        if options.ids.as_ref().is_some_and(|ids| ids.is_empty() || ids.len() > 100_000) {
            return Err(ServiceError::Invalid(
                "export IDs must contain between 1 and 100000 items".into(),
            ));
        }
        let id = Uuid::new_v4();
        let directory = self.pending_directory(id).await?;
        self.enqueue(id, Work::Export(options), idempotency_key, directory).await
    }

    /// Queue the creation of new image variants from existing media.
    pub async fn submit_process(
        &self,
        id: Uuid,
        options: ProcessOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        self.validate_upload(&UploadOptions {
            kind: MediaKind::Image,
            image: options.image.clone(),
            ..UploadOptions::default()
        })?;
        let source = self.get_media(id).await?.ok_or(ServiceError::NotFound)?;
        if source.single_use {
            return Err(ServiceError::Conflict("single-use media cannot be reprocessed".into()));
        }
        let content = self
            .open_content(
                id,
                super::ContentRequest {
                    variant: Some("original".into()),
                    ..super::ContentRequest::default()
                },
                true,
            )
            .await?;
        let file_name = content.metadata.file_name.clone();
        let mut content = content;
        let (task_id, directory, hash) = self.stage_input(&mut content.file).await?;
        self.enqueue(
            task_id,
            Work::Process {
                source: id,
                options,
                hash,
                file_name,
                expires_at: source.expires_at,
            },
            idempotency_key,
            directory,
        )
        .await
    }

    fn validate_upload(&self, options: &UploadOptions) -> Result<(), ServiceError> {
        if matches!(options.kind, MediaKind::Audio | MediaKind::Video) {
            return Err(ServiceError::Unsupported(
                "audio and video processing are not available".into(),
            ));
        }
        if options.retention.expires_in_seconds.is_some_and(|v| v == 0 || v > 36_000_000) {
            return Err(ServiceError::Invalid(
                "retention must be between 1 second and 10000 hours".into(),
            ));
        }
        if let Some(name) = &options.file_name
            && (name.len() > 512 || name.chars().any(char::is_control))
        {
            return Err(ServiceError::Invalid("invalid file name".into()));
        }
        if let Some(file_type) = &options.file_type {
            crate::mime::Mime::from_str(file_type)
                .map_err(|_| ServiceError::Invalid("invalid MIME type".into()))?;
        }
        if options.kind == MediaKind::Image {
            #[cfg(feature = "image-convert")]
            super::image_processor::validate_options(&options.image, &self.0.config.image_limits)?;
            #[cfg(not(feature = "image-convert"))]
            return Err(ServiceError::Unsupported("image processing is disabled".into()));
        }
        Ok(())
    }

    async fn pending_directory(&self, id: Uuid) -> Result<PendingDirectory, ServiceError> {
        if self.0.shutdown.load(Ordering::Acquire) {
            return Err(ServiceError::Busy);
        }
        let path = self.work_directory(id);
        fs::create_dir_all(
            path.parent().ok_or_else(|| ServiceError::Internal("invalid work directory".into()))?,
        )
        .await?;
        fs::create_dir(&path).await?;
        Ok(PendingDirectory {
            path,
            committed: false,
        })
    }

    async fn stage_input(
        &self,
        mut reader: impl AsyncRead + Unpin,
    ) -> Result<(Uuid, PendingDirectory, String), ServiceError> {
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let id = Uuid::new_v4();
        let directory = self.pending_directory(id).await?;
        let mut file = fs::File::create(directory.path.join("input")).await?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let n = reader.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            bytes = bytes.checked_add(n as u64).ok_or(ServiceError::PayloadTooLarge)?;
            if bytes > self.0.config.max_file_size {
                return Err(ServiceError::PayloadTooLarge);
            }
            file.write_all(&buffer[..n]).await?;
            hash.update(&buffer[..n]);
        }
        file.flush().await?;
        file.sync_all().await?;
        sync_directory(&directory.path).await?;
        sync_directory(
            directory
                .path
                .parent()
                .ok_or_else(|| ServiceError::Internal("invalid work path".into()))?,
        )
        .await?;
        Ok((id, directory, hex::encode(hash.finalize())))
    }

    async fn enqueue(
        &self,
        id: Uuid,
        work: Work,
        key: Option<String>,
        directory: PendingDirectory,
    ) -> Result<Task, ServiceError> {
        if key.as_ref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 128
                || key.bytes().any(|byte| !(0x20..=0x7E).contains(&byte))
        }) {
            return Err(ServiceError::Invalid(
                "Idempotency-Key must contain 1 to 128 printable bytes".into(),
            ));
        }
        let service = self.clone();
        // Keep the input until SQLite saves the task, even if the HTTP request ends.
        tokio::spawn(async move {
            let mut directory = directory;
            let _gate = service.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
            let _mutation = service.0.mutations.lock().await;
            let payload = serde_json::to_string(&work)?;
            let fingerprint = hex::encode(Sha256::digest(payload.as_bytes()));
            if let Some(key) = &key {
                let existing = sqlx::query(
                    "SELECT metadata, fingerprint FROM tasks WHERE idempotency_key = ?",
                )
                .bind(key)
                .fetch_optional(&service.0.datalith.0.db)
                .await?;
                if let Some(row) = existing {
                    if row.try_get::<String, _>("fingerprint")? != fingerprint {
                        return Err(ServiceError::Conflict(
                            "Idempotency-Key was already used with a different request".into(),
                        ));
                    }
                    return Ok(serde_json::from_str(row.try_get("metadata")?)?);
                }
            }
            let now = Utc::now();
            let task = Task {
                id,
                kind: match &work {
                    Work::Upload {
                        options, ..
                    } if options.kind == MediaKind::Image => "image",
                    Work::Upload {
                        ..
                    } => "resource",
                    Work::Process {
                        ..
                    } => "image",
                    Work::Import {
                        ..
                    } => "import",
                    Work::Export(_) => "export",
                }
                .into(),
                status: TaskStatus::Queued,
                stage: "queued".into(),
                completed_units: 0,
                total_units: None,
                attempt: 0,
                created_at: now,
                updated_at: now,
                result: None,
                error: None,
            };
            sqlx::query(
                "INSERT INTO \
                 tasks(id,status,created_at,updated_at,metadata,work,idempotency_key,fingerprint) \
                 VALUES(?, 'queued', ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(now.timestamp_millis())
            .bind(now.timestamp_millis())
            .bind(serde_json::to_string(&task)?)
            .bind(payload)
            .bind(key)
            .bind(fingerprint)
            .execute(&service.0.datalith.0.db)
            .await?;
            directory.committed = true;
            drop(directory);
            service.0.wakeup.notify_one();
            Ok(task)
        })
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))?
    }

    /// Get a task by ID.
    pub async fn get_task(&self, id: Uuid) -> Result<Option<Task>, ServiceError> {
        let metadata: Option<String> = sqlx::query_scalar("SELECT metadata FROM tasks WHERE id=?")
            .bind(id)
            .fetch_optional(&self.0.datalith.0.db)
            .await?;
        metadata.map(|value| serde_json::from_str(&value).map_err(ServiceError::from)).transpose()
    }

    /// Cancel a task; a running task stops at its next safe point.
    pub async fn cancel_task(&self, id: Uuid) -> Result<Task, ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if task.status.is_terminal() {
            return Ok(task);
        }
        task.status = if task.status == TaskStatus::Queued {
            TaskStatus::Cancelled
        } else {
            TaskStatus::Cancelling
        };
        task.stage =
            if task.status == TaskStatus::Cancelled { "cancelled" } else { "cancelling" }.into();
        self.save_task(&mut task).await?;
        if let Some(cancel) = self.0.cancellations.lock().unwrap().get(&id) {
            cancel.store(true, Ordering::Release);
        }
        self.0.wakeup.notify_waiters();
        Ok(task)
    }

    /// Queue a failed or cancelled task again.
    pub async fn retry_task(&self, id: Uuid) -> Result<Task, ServiceError> {
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
            return Err(ServiceError::Conflict(
                "only failed or cancelled tasks can be retried".into(),
            ));
        }
        if task.kind != "export" && !fs::try_exists(self.work_directory(id).join("input")).await? {
            return Err(ServiceError::NotFound);
        }
        task.status = TaskStatus::Queued;
        task.stage = "queued".into();
        task.error = None;
        task.result = None;
        task.completed_units = 0;
        self.save_task(&mut task).await?;
        self.0.wakeup.notify_one();
        Ok(task)
    }

    async fn save_task(&self, task: &mut Task) -> Result<(), ServiceError> {
        task.updated_at = Utc::now();
        sqlx::query("UPDATE tasks SET status=?, updated_at=?, metadata=? WHERE id=?")
            .bind(status_name(task.status))
            .bind(task.updated_at.timestamp_millis())
            .bind(serde_json::to_string(task)?)
            .bind(task.id)
            .execute(&self.0.datalith.0.db)
            .await?;
        Ok(())
    }

    pub(super) async fn complete_task_tx(
        tx: &mut Transaction<'_, Sqlite>,
        id: Uuid,
        result: &serde_json::Value,
    ) -> Result<(), ServiceError> {
        let value: String = sqlx::query_scalar("SELECT metadata FROM tasks WHERE id=?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        let mut task: Task = serde_json::from_str(&value)?;
        if task.status == TaskStatus::Cancelling || task.status == TaskStatus::Cancelled {
            return Err(ServiceError::Cancelled);
        }
        task.status = TaskStatus::Succeeded;
        task.stage = "complete".into();
        task.completed_units = task.total_units.unwrap_or(1);
        task.total_units = Some(task.completed_units);
        task.updated_at = Utc::now();
        task.result = Some(result.clone());
        task.error = None;
        sqlx::query("UPDATE tasks SET status='succeeded', updated_at=?, metadata=? WHERE id=?")
            .bind(task.updated_at.timestamp_millis())
            .bind(serde_json::to_string(&task)?)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub(super) async fn recover_tasks(&self) -> Result<(), ServiceError> {
        let rows =
            sqlx::query("SELECT metadata FROM tasks WHERE status IN ('running','cancelling')")
                .fetch_all(&self.0.datalith.0.db)
                .await?;
        for row in rows {
            let mut task: Task = serde_json::from_str(row.try_get("metadata")?)?;
            task.status = if task.status == TaskStatus::Cancelling {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Queued
            };
            task.stage =
                if task.status == TaskStatus::Queued { "recovered" } else { "cancelled" }.into();
            self.save_task(&mut task).await?;
        }
        let root = self.0.datalith.get_environment().join("datalith.tasks");
        fs::create_dir_all(&root).await?;
        sync_directory(self.0.datalith.get_environment()).await?;
        let mut entries = fs::read_dir(&root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(id) = entry.file_name().to_str().and_then(|name| Uuid::parse_str(name).ok())
            else {
                continue;
            };
            if entry.file_type().await?.is_dir() {
                if self.get_task(id).await?.is_none() {
                    fs::remove_dir_all(entry.path()).await?;
                } else {
                    self.clean_task_files(id, true).await?;
                }
            }
        }
        Ok(())
    }

    async fn claim_task(&self) -> Result<Option<(Task, Work, Arc<AtomicBool>)>, ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT metadata,work FROM tasks WHERE status='queued' ORDER BY created_at,id LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut task: Task = serde_json::from_str(row.try_get("metadata")?)?;
        let work: Work = serde_json::from_str(row.try_get("work")?)?;
        task.status = TaskStatus::Running;
        task.stage = if task.kind == "image" { "processing" } else { "storing" }.into();
        task.attempt = task.attempt.saturating_add(1);
        task.updated_at = Utc::now();
        task.total_units = Some(1);
        let cancel = Arc::new(AtomicBool::new(false));
        sqlx::query("UPDATE tasks SET status='running',updated_at=?,metadata=? WHERE id=?")
            .bind(task.updated_at.timestamp_millis())
            .bind(serde_json::to_string(&task)?)
            .bind(task.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.0.cancellations.lock().unwrap().insert(task.id, cancel.clone());
        Ok(Some((task, work, cancel)))
    }

    pub(super) async fn worker(inner: std::sync::Weak<super::ServiceInner>) {
        loop {
            let Some(inner) = inner.upgrade() else {
                break;
            };
            let service = Self(inner);
            if service.0.shutdown.load(Ordering::Acquire) {
                break;
            }
            let wakeup = service.0.wakeup.clone();
            let notified = wakeup.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match service.claim_task().await {
                Ok(Some((task, work, cancel))) => {
                    let id = task.id;
                    let result = match work {
                        Work::Upload {
                            options, ..
                        } => service.run_upload(id, options, None, cancel.clone()).await,
                        Work::Process {
                            options,
                            file_name,
                            expires_at,
                            ..
                        } => {
                            service
                                .run_upload(
                                    id,
                                    UploadOptions {
                                        kind: MediaKind::Image,
                                        file_name: Some(file_name),
                                        image: options.image,
                                        ..UploadOptions::default()
                                    },
                                    expires_at,
                                    cancel.clone(),
                                )
                                .await
                        },
                        Work::Export(options) => {
                            service.export_archive(id, options, cancel.clone()).await
                        },
                        Work::Import {
                            ..
                        } => service.import_archive(id, cancel.clone()).await,
                    };
                    if let Err(error) = service.finish_task(id, result).await {
                        tracing::error!(task_id=%id, %error, "cannot record task outcome");
                    }
                    if let Err(error) = service.clean_task_files(id, false).await {
                        tracing::warn!(task_id=%id, %error, "task staging cleanup failed");
                    }
                    let mut active = service.0.cancellations.lock().unwrap();
                    if active.get(&id).is_some_and(|current| Arc::ptr_eq(current, &cancel)) {
                        active.remove(&id);
                    }
                    continue;
                },
                Ok(None) => (),
                Err(error) => tracing::error!(%error, "cannot claim task"),
            }
            drop(service);
            tokio::select! { _ = notified => (), _ = tokio::time::sleep(Duration::from_secs(1)) => () }
        }
    }

    pub(super) async fn maintenance(inner: std::sync::Weak<super::ServiceInner>) {
        let mut last_scan = tokio::time::Instant::now();
        loop {
            let Some(service) = inner.upgrade().map(Self) else {
                break;
            };
            let stopping = service.0.stopping.clone();
            let notified = stopping.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if service.0.shutdown.load(Ordering::Acquire) {
                break;
            }
            // Do not keep the service alive while waiting.
            drop(service);
            tokio::select! { _ = notified => continue, _ = tokio::time::sleep(MAINTENANCE_INTERVAL) => () }
            let Some(service) = inner.upgrade().map(Self) else {
                break;
            };
            if service.0.shutdown.load(Ordering::Acquire) {
                break;
            }
            if let Err(error) = service.collect_garbage().await {
                tracing::warn!(%error, "content cleanup failed");
            }
            if let Err(error) = service.expire_tasks().await {
                tracing::warn!(%error, "task cleanup failed");
            }
            if let Err(error) = service.clear_released_files().await {
                tracing::warn!(%error, "released file cleanup failed");
            }
            // Keep a full scan for files left by interrupted work or older storage methods.
            if last_scan.elapsed() >= FULL_SCAN_INTERVAL {
                if let Err(error) = service.clear_untracked_files().await {
                    tracing::warn!(%error, "stored file cleanup failed");
                }
                last_scan = tokio::time::Instant::now();
            }
        }
    }

    async fn finish_task(
        &self,
        id: Uuid,
        result: Result<serde_json::Value, ServiceError>,
    ) -> Result<(), ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if task.status.is_terminal() {
            return Ok(());
        }
        match result {
            Ok(result) => {
                let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
                if task.status == TaskStatus::Cancelling {
                    task.status = TaskStatus::Cancelled;
                    drop(tx);
                    self.save_task(&mut task).await?;
                } else {
                    Self::complete_task_tx(&mut tx, id, &result).await?;
                    tx.commit().await?;
                }
            },
            Err(error) => {
                task.status = if matches!(error, ServiceError::Cancelled)
                    || task.status == TaskStatus::Cancelling
                {
                    TaskStatus::Cancelled
                } else {
                    TaskStatus::Failed
                };
                task.stage = status_name(task.status).into();
                task.error = Some(TaskError {
                    code:    error.code().into(),
                    message: error.to_string(),
                });
                self.save_task(&mut task).await?;
            },
        }
        Ok(())
    }

    async fn clean_task_files(&self, id: Uuid, recovering: bool) -> Result<(), ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let Some(task) = self.get_task(id).await? else {
            return Ok(());
        };
        if !task.status.is_terminal() && !recovering {
            return Ok(());
        }
        let mut entries = match fs::read_dir(self.work_directory(id)).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            if (name == "input" && task.status != TaskStatus::Succeeded)
                || (name == "export.tar"
                    && task.kind == "export"
                    && task.status == TaskStatus::Succeeded)
            {
                continue;
            }
            if entry.file_type().await?.is_dir() {
                fs::remove_dir_all(entry.path()).await?;
            } else {
                fs::remove_file(entry.path()).await?;
            }
        }
        Ok(())
    }

    async fn expire_tasks(&self) -> Result<(), ServiceError> {
        let Ok(_artifacts) = self.0.artifacts.try_write() else {
            return Ok(());
        };
        let cutoff = Utc::now().timestamp_millis().saturating_sub(
            i64::try_from(self.0.config.task_retention_seconds.saturating_mul(1000))
                .unwrap_or(i64::MAX),
        );
        let mutation = self.0.mutations.lock().await;
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "DELETE FROM tasks WHERE status IN ('succeeded','failed','cancelled') AND updated_at \
             < ? RETURNING id",
        )
        .bind(cutoff)
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        // The tasks are gone, so nothing else uses their directories.
        drop(mutation);
        for id in ids {
            match fs::remove_dir_all(self.work_directory(id)).await {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    async fn run_upload(
        &self,
        id: Uuid,
        options: UploadOptions,
        source_expiry: Option<chrono::DateTime<Utc>>,
        cancel: Arc<AtomicBool>,
    ) -> Result<serde_json::Value, ServiceError> {
        let input = self.work_directory(id).join("input");
        let output = self.work_directory(id).join("output");
        if fs::try_exists(&output).await? {
            fs::remove_dir_all(&output).await?;
        }
        fs::create_dir(&output).await?;
        let created_at = Utc::now();
        let name = options
            .file_name
            .clone()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| id.to_string());
        let mut media = Media {
            id,
            kind: options.kind,
            created_at,
            file_name: name.clone(),
            original: None,
            variants: Vec::new(),
            expires_at: None,
            single_use: options.retention.single_use,
            consumed_at: None,
            animated: false,
            frame_count: 1,
        };
        let mut prepared = HashMap::new();
        let mut mime =
            options.file_type.clone().unwrap_or_else(|| "application/octet-stream".into());
        if options.kind == MediaKind::Image {
            #[cfg(feature = "image-convert")]
            {
                let image_input = input.clone();
                let image_options = options.image.clone();
                let limits = self.0.config.image_limits.clone();
                let image_cancel = cancel.clone();
                let image = tokio::task::spawn_blocking(move || {
                    super::image_processor::process_image(
                        &image_input,
                        &output,
                        &image_options,
                        &limits,
                        &image_cancel,
                    )
                })
                .await
                .map_err(|e| ServiceError::Internal(e.to_string()))??;
                media.animated = image.animated;
                media.frame_count = image.frame_count;
                mime = image.original_mime;
                // Each variant file gets its own extension, so drop the one from the original name.
                let stem = std::path::Path::new(&name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map_or(name.as_str(), |extension| &name[..name.len() - extension.len() - 1]);
                for variant in image.variants {
                    let file = Self::prepare_file(
                        variant.path,
                        variant.mime,
                        format!(
                            "{}-{}@{}x.{}",
                            stem, variant.spec.name, variant.multiplier, variant.format
                        ),
                    )
                    .await?;
                    media.variants.push(Variant {
                        name:         variant.spec.name.clone(),
                        multiplier:   variant.multiplier,
                        format:       variant.format.clone(),
                        width:        variant.width,
                        height:       variant.height,
                        animated:     variant.animated,
                        file:         file.metadata.clone(),
                        content_path: content_path(
                            id,
                            &variant.spec.name,
                            variant.multiplier,
                            &variant.format,
                        ),
                        recipe:       Some(variant.spec),
                    });
                    prepared.insert(file.metadata.id, file);
                }
            }
            #[cfg(not(feature = "image-convert"))]
            return Err(ServiceError::Unsupported("image processing is disabled".into()));
        } else if options.file_type.is_none()
            && let Some(detected) = crate::functions::detect_file_type_by_path(&input, false).await
        {
            mime = detected.to_string();
        }
        if options.kind == MediaKind::Resource || options.image.save_original {
            let file = Self::prepare_file(input, mime, name).await?;
            media.original = Some(file.metadata.clone());
            prepared.insert(file.metadata.id, file);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(ServiceError::Cancelled);
        }
        let _gate = self.0.writes.read().await;
        let _mutation = self.0.mutations.lock().await;
        if cancel.load(Ordering::Acquire) {
            return Err(ServiceError::Cancelled);
        }
        media.created_at = Utc::now();
        media.expires_at = source_expiry.or_else(|| {
            options
                .retention
                .expires_in_seconds
                .map(|seconds| media.created_at + chrono::Duration::seconds(seconds as i64))
        });
        if media.expires_at.is_some_and(|expiry| expiry <= media.created_at) {
            return Err(ServiceError::NotFound);
        }
        let mut guards = Vec::new();
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        self.publish_media_tx(&mut tx, &mut media, &prepared, &mut guards).await?;
        let result = serde_json::to_value(&media)?;
        Self::complete_task_tx(&mut tx, id, &result).await?;
        tx.commit().await?;
        drop(guards);
        Ok(result)
    }
}

fn status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Succeeded => "succeeded",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}
