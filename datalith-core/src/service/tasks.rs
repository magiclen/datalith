use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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
    DatalithService, ExportOptions, Media, MediaFile, MediaKind, PreparedFile, ProcessOptions,
    ProcessingRecipe, ServiceError, StagedInput, Task, TaskError, TaskStatus, UploadOptions, Work,
    store::sync_directory,
};
#[cfg(feature = "image-convert")]
use super::{Variant, migration::content_path};

const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(60 * 60);
// A task which is still running at this many starts in a row is failed instead of being run again.
const MAX_CRASH_RECOVERIES: i64 = 3;

pub(super) struct PendingDirectory {
    pub path:  PathBuf,
    committed: bool,
}

impl Drop for PendingDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

impl PendingDirectory {
    async fn sync(&self) -> Result<(), ServiceError> {
        sync_directory(&self.path).await?;
        sync_directory(
            self.path.parent().ok_or_else(|| ServiceError::Internal("invalid work path".into()))?,
        )
        .await
    }
}

type StagedTask = (Uuid, PendingDirectory, StagedInput);

impl DatalithService {
    /// Queue an upload; the returned task creates the media.
    /// A repeated request with the same idempotency key and content returns the first task.
    /// It returns `Busy` if an export is waiting or taking its snapshot when it is called, but an export never blocks or discards an input that has been accepted.
    pub async fn submit_upload(
        &self,
        reader: impl AsyncRead + Unpin,
        options: UploadOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        validate_idempotency_key(idempotency_key.as_deref())?;
        self.validate_upload(&options)?;
        let staged = self.stage_input(reader).await?;
        self.enqueue_upload(staged, options, idempotency_key).await
    }

    /// Queue an upload from a file, like `submit_upload`.
    /// The file is linked instead of copied when the file system allows it, so do not change it after this call; removing it is fine.
    pub async fn submit_upload_file(
        &self,
        path: impl AsRef<Path>,
        options: UploadOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        validate_idempotency_key(idempotency_key.as_deref())?;
        self.validate_upload(&options)?;
        let staged = self.stage_file(path.as_ref()).await?;
        self.enqueue_upload(staged, options, idempotency_key).await
    }

    async fn enqueue_upload(
        &self,
        (id, directory, input): StagedTask,
        options: UploadOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        self.enqueue(
            id,
            Work::Upload {
                options,
                recipe: Some(ProcessingRecipe::from_config(&self.0.config)),
                input,
            },
            idempotency_key,
            directory,
        )
        .await
    }

    /// Queue the import of a Datalith archive.
    /// It returns `Busy` if an export is waiting or taking its snapshot when it is called, but an export never blocks or discards an input that has been accepted.
    pub async fn submit_import(
        &self,
        reader: impl AsyncRead + Unpin,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        validate_idempotency_key(idempotency_key.as_deref())?;
        let staged = self.stage_input(reader).await?;
        self.enqueue_import(staged, idempotency_key).await
    }

    /// Queue the import of a Datalith archive file, like `submit_import`.
    /// The file is linked instead of copied when the file system allows it, so do not change it after this call; removing it is fine.
    pub async fn submit_import_file(
        &self,
        path: impl AsRef<Path>,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        validate_idempotency_key(idempotency_key.as_deref())?;
        let staged = self.stage_file(path.as_ref()).await?;
        self.enqueue_import(staged, idempotency_key).await
    }

    async fn enqueue_import(
        &self,
        (id, directory, input): StagedTask,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        self.enqueue(
            id,
            Work::Import {
                hash: input.hash
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
        validate_idempotency_key(idempotency_key.as_deref())?;
        if options.ids.as_ref().is_some_and(|ids| ids.is_empty() || ids.len() > 100_000) {
            return Err(ServiceError::Invalid(
                "export IDs must contain between 1 and 100000 items".into(),
            ));
        }
        let id = Uuid::new_v4();
        let directory = self.pending_directory(id).await?;
        self.enqueue(id, Work::Export(options), idempotency_key, directory).await
    }

    /// Queue processing of the retained original into a new media item.
    pub async fn submit_process(
        &self,
        id: Uuid,
        options: ProcessOptions,
        idempotency_key: Option<String>,
    ) -> Result<Task, ServiceError> {
        validate_idempotency_key(idempotency_key.as_deref())?;
        if let Some(key) = &idempotency_key
            && let Some(task) =
                self.find_idempotent_task(key, &process_fingerprint(id, &options)?).await?
        {
            return Ok(task);
        }
        if options.kind == MediaKind::Resource {
            return Err(ServiceError::Invalid(
                "reprocessing requires image, audio, or video options".into(),
            ));
        }
        self.validate_upload(&UploadOptions {
            kind: options.kind,
            image: options.image.clone(),
            audio: options.audio.clone(),
            video: options.video.clone(),
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
        let file_size = content
            .metadata
            .file_size
            .parse()
            .map_err(|_| ServiceError::Internal("invalid stored file size".into()))?;
        let stored = self.0.datalith.get_file_path(content.metadata.id).await?;
        // Stored files never change, so the task can share the retained original instead of copying it.
        let linked =
            self.link_input(&stored, Some((content.metadata.sha256.clone(), file_size))).await?;
        let (task_id, directory, input) = match linked {
            Some(staged) => staged,
            None => {
                let mut content = content;
                self.stage_input(&mut content.file).await?
            },
        };
        self.enqueue(
            task_id,
            Work::Process {
                source: id,
                options,
                recipe: Some(ProcessingRecipe::from_config(&self.0.config)),
                input,
                file_name,
                expires_at: source.expires_at,
            },
            idempotency_key,
            directory,
        )
        .await
    }

    fn validate_upload(&self, options: &UploadOptions) -> Result<(), ServiceError> {
        if options.automatic() && options.kind != MediaKind::Resource {
            return Err(ServiceError::Invalid(
                "automatic conversion flags cannot be combined with an explicit media kind".into(),
            ));
        }
        let audio = options.kind == MediaKind::Audio || options.enable_convert_to_audio;
        let video = options.kind == MediaKind::Video || options.enable_convert_to_video;
        if audio || video {
            #[cfg(feature = "av-convert")]
            {
                if !self.0.av.available {
                    return Err(ServiceError::Unsupported(
                        self.0
                            .av
                            .reason
                            .clone()
                            .unwrap_or_else(|| "audio and video processing are unavailable".into()),
                    ));
                }
                if audio && !self.0.av.audio_available || video && !self.0.av.video_available {
                    return Err(ServiceError::Unsupported(
                        "the required media encoder or timestamp filter is unavailable".into(),
                    ));
                }
                let mut enabled = options.clone();
                if video {
                    enabled.kind = MediaKind::Video;
                }
                super::av_processor::validate_options(&enabled)?;
            }
            #[cfg(not(feature = "av-convert"))]
            return Err(ServiceError::Unsupported(
                "audio and video processing are disabled".into(),
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
        if options.kind == MediaKind::Image || options.enable_convert_to_image {
            #[cfg(feature = "image-convert")]
            super::image_processor::validate_options(&options.image, &self.0.config.image_limits)?;
            #[cfg(not(feature = "image-convert"))]
            return Err(ServiceError::Unsupported("image processing is disabled".into()));
        }
        Ok(())
    }

    pub(super) async fn pending_directory(
        &self,
        id: Uuid,
    ) -> Result<PendingDirectory, ServiceError> {
        if self.0.shutdown.load(Ordering::Acquire) {
            return Err(ServiceError::Busy);
        }
        // Fail fast while an export is pending, but do not hold the gate, because staging never touches stored content.
        self.0.writes.try_read().map(drop).map_err(|_| ServiceError::Busy)?;
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
    ) -> Result<StagedTask, ServiceError> {
        let id = Uuid::new_v4();
        let directory = self.pending_directory(id).await?;
        let mut file = fs::File::create(directory.path.join("input")).await?;
        let (hash, bytes) = self.read_input(&mut reader, Some(&mut file)).await?;
        file.flush().await?;
        file.sync_all().await?;
        directory.sync().await?;
        Ok((id, directory, StagedInput {
            hash,
            file_size: Some(bytes),
        }))
    }

    async fn stage_file(&self, path: &Path) -> Result<StagedTask, ServiceError> {
        match self.link_input(path, None).await? {
            Some(staged) => Ok(staged),
            None => self.stage_input(fs::File::open(path).await?).await,
        }
    }

    // Staged inputs are never changed, so a task can share the content of a file through a hard link.
    // Return `None` when the file cannot be linked, so that the caller can copy it instead.
    async fn link_input(
        &self,
        source: &Path,
        known: Option<(String, u64)>,
    ) -> Result<Option<StagedTask>, ServiceError> {
        // A hard link to a symbolic link does not follow it, so only link regular files.
        let metadata = match fs::symlink_metadata(source).await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => return Ok(None),
        };
        if metadata.len() > self.0.config.max_file_size {
            return Err(ServiceError::PayloadTooLarge);
        }
        let id = Uuid::new_v4();
        let directory = self.pending_directory(id).await?;
        let input = directory.path.join("input");
        if fs::hard_link(source, &input).await.is_err() {
            return Ok(None);
        }
        let (hash, bytes) = match known {
            Some(known) => known,
            None => {
                let mut file = fs::File::open(&input).await?;
                let read = self.read_input(&mut file, None).await?;
                file.sync_all().await?;
                read
            },
        };
        directory.sync().await?;
        Ok(Some((id, directory, StagedInput {
            hash,
            file_size: Some(bytes),
        })))
    }

    // Hash the input and check its size, and copy it into `output` when it is given.
    async fn read_input(
        &self,
        reader: &mut (impl AsyncRead + Unpin),
        mut output: Option<&mut fs::File>,
    ) -> Result<(String, u64), ServiceError> {
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
            if let Some(output) = output.as_mut() {
                output.write_all(&buffer[..n]).await?;
            }
            hash.update(&buffer[..n]);
        }
        Ok((hex::encode(hash.finalize()), bytes))
    }

    pub(super) async fn find_idempotent_task(
        &self,
        key: &str,
        fingerprint: &str,
    ) -> Result<Option<Task>, ServiceError> {
        let row = sqlx::query("SELECT metadata, work FROM tasks WHERE idempotency_key = ?")
            .bind(key)
            .fetch_optional(&self.0.datalith.0.db)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let work: Work = serde_json::from_str(row.try_get("work")?)?;
        if request_fingerprint(&work)? != fingerprint {
            return Err(ServiceError::Conflict(
                "Idempotency-Key was already used with a different request".into(),
            ));
        }
        Ok(Some(serde_json::from_str(row.try_get("metadata")?)?))
    }

    pub(super) async fn enqueue(
        &self,
        id: Uuid,
        work: Work,
        key: Option<String>,
        directory: PendingDirectory,
    ) -> Result<Task, ServiceError> {
        let service = self.clone();
        // Keep the input until SQLite saves the task, even if the HTTP request ends.
        tokio::spawn(async move {
            let mut directory = directory;
            // The export snapshot holds `mutations`, so saving a task only waits for that short step.
            let _mutation = service.0.mutations.lock().await;
            let payload = serde_json::to_string(&work)?;
            let fingerprint = request_fingerprint(&work)?;
            if let Some(key) = &key
                && let Some(task) = service.find_idempotent_task(key, &fingerprint).await?
            {
                return Ok(task);
            }
            let now = Utc::now();
            let task = Task {
                id,
                kind: match &work {
                    Work::Upload {
                        options, ..
                    } if options.automatic() => "upload",
                    Work::Upload {
                        options, ..
                    } => match options.kind {
                        MediaKind::Resource => "resource",
                        MediaKind::Image => "image",
                        MediaKind::Audio => "audio",
                        MediaKind::Video => "video",
                    },
                    Work::Process {
                        options, ..
                    } => match options.kind {
                        MediaKind::Resource => "resource",
                        MediaKind::Image => "image",
                        MediaKind::Audio => "audio",
                        MediaKind::Video => "video",
                    },
                    Work::Import {
                        ..
                    } => "import",
                    Work::Export(_) => "export",
                    Work::Mp4Export(_) => "mp4_export",
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
        self.cancel_task_with_session(id, None).await
    }

    /// Cancel a task with the credential required by a single-use MP4 export.
    pub async fn cancel_task_with_session(
        &self,
        id: Uuid,
        session: Option<&str>,
    ) -> Result<Task, ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if task.kind == "mp4_export" {
            self.verify_export_authorization(&self.saved_mp4_work(id).await?, session).await?;
        }
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
        self.retry_task_with_session(id, None).await
    }

    /// Retry a task with the credential required by a single-use MP4 export.
    pub async fn retry_task_with_session(
        &self,
        id: Uuid,
        session: Option<&str>,
    ) -> Result<Task, ServiceError> {
        let _gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
            return Err(ServiceError::Conflict(
                "only failed or cancelled tasks can be retried".into(),
            ));
        }
        if task.kind == "mp4_export" {
            let work = self.saved_mp4_work(id).await?;
            self.verify_export_authorization(&work, session).await?;
            self.validate_export_work(&work).await?;
            if !fs::try_exists(self.work_directory(id).join("snapshot")).await? {
                return Err(ServiceError::NotFound);
            }
        } else if task.kind != "export"
            && !fs::try_exists(self.work_directory(id).join("input")).await?
        {
            return Err(ServiceError::NotFound);
        }
        task.status = TaskStatus::Queued;
        task.stage = "queued".into();
        task.error = None;
        task.result = None;
        task.completed_units = 0;
        sqlx::query("UPDATE tasks SET crash_count=0 WHERE id=?")
            .bind(id)
            .execute(&self.0.datalith.0.db)
            .await?;
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

    pub(super) async fn processing_progress(
        &self,
        id: Uuid,
        stage: &str,
        completed: u64,
        total: u64,
    ) -> Result<(), ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let mut task = self.get_task(id).await?.ok_or(ServiceError::NotFound)?;
        if task.status == TaskStatus::Cancelling || task.status == TaskStatus::Cancelled {
            return Err(ServiceError::Cancelled);
        }
        if task.status != TaskStatus::Running {
            return Ok(());
        }
        task.stage = stage.into();
        task.completed_units = completed.min(total);
        task.total_units = (total != 0).then_some(total);
        self.save_task(&mut task).await
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
        let rows = sqlx::query(
            "SELECT metadata, crash_count FROM tasks WHERE status IN ('running','cancelling')",
        )
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        for row in rows {
            let mut task: Task = serde_json::from_str(row.try_get("metadata")?)?;
            let mut crash_count: i64 = row.try_get("crash_count")?;
            if task.status == TaskStatus::Cancelling {
                task.status = TaskStatus::Cancelled;
                task.stage = "cancelled".into();
            } else {
                // A normal shutdown puts interrupted tasks back into the queue, so a running task means that the process stopped unexpectedly.
                crash_count = crash_count.saturating_add(1);
                if crash_count >= MAX_CRASH_RECOVERIES {
                    task.status = TaskStatus::Failed;
                    task.stage = status_name(task.status).into();
                    task.error = Some(TaskError {
                        code:    "repeated_interruption".into(),
                        message: format!(
                            "The service stopped unexpectedly {crash_count} times while running \
                             this task, so it was not started again. Retry it after the cause is \
                             fixed."
                        ),
                    });
                } else {
                    task.status = TaskStatus::Queued;
                    task.stage = "recovered".into();
                }
            }
            task.updated_at = Utc::now();
            sqlx::query(
                "UPDATE tasks SET status=?, updated_at=?, metadata=?, crash_count=? WHERE id=?",
            )
            .bind(status_name(task.status))
            .bind(task.updated_at.timestamp_millis())
            .bind(serde_json::to_string(&task)?)
            .bind(crash_count)
            .bind(task.id)
            .execute(&self.0.datalith.0.db)
            .await?;
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

    // Claim the oldest queued task, or only the given one.
    async fn claim_task(
        &self,
        only: Option<Uuid>,
    ) -> Result<Option<(Task, Work, Arc<AtomicBool>)>, ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        // Shutdown puts interrupted tasks back into the queue, so they must not be claimed again here.
        if self.0.shutdown.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT metadata,work FROM tasks WHERE status='queued' AND (? IS NULL OR id=?) ORDER \
             BY created_at,id LIMIT 1",
        )
        .bind(only)
        .bind(only)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut task: Task = serde_json::from_str(row.try_get("metadata")?)?;
        let mut work: Work = serde_json::from_str(row.try_get("work")?)?;
        // Freeze legacy recipes before the first attempt with this service version.
        match &mut work {
            Work::Upload {
                recipe, ..
            }
            | Work::Process {
                recipe, ..
            } => {
                if recipe.is_none() {
                    *recipe = Some(ProcessingRecipe::from_config(&self.0.config));
                }
            },
            Work::Import {
                ..
            }
            | Work::Export(_) => (),
            Work::Mp4Export(_) => (),
        }
        task.status = TaskStatus::Running;
        task.stage = if matches!(task.kind.as_str(), "image" | "audio" | "video") {
            "processing"
        } else {
            "storing"
        }
        .into();
        task.attempt = task.attempt.saturating_add(1);
        task.updated_at = Utc::now();
        task.total_units = Some(1);
        let cancel = Arc::new(AtomicBool::new(false));
        sqlx::query("UPDATE tasks SET status='running',updated_at=?,metadata=?,work=? WHERE id=?")
            .bind(task.updated_at.timestamp_millis())
            .bind(serde_json::to_string(&task)?)
            .bind(serde_json::to_string(&work)?)
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
            match service.claim_task(None).await {
                Ok(Some((task, work, cancel))) => {
                    service.run_claimed(task, work, cancel).await;
                    continue;
                },
                Ok(None) => (),
                Err(error) => tracing::error!(%error, "cannot claim task"),
            }
            drop(service);
            tokio::select! { _ = notified => (), _ = tokio::time::sleep(Duration::from_secs(1)) => () }
        }
    }

    // Run a claimed task, record its outcome, and clean up its staging files.
    async fn run_claimed(&self, task: Task, work: Work, cancel: Arc<AtomicBool>) {
        let id = task.id;
        let result = match work {
            Work::Upload {
                options,
                recipe,
                input,
            } => self.run_upload(id, options, recipe.unwrap(), None, input, cancel.clone()).await,
            Work::Process {
                options,
                recipe,
                file_name,
                expires_at,
                input,
                ..
            } => {
                self.run_upload(
                    id,
                    UploadOptions {
                        kind: options.kind,
                        file_name: Some(file_name),
                        image: options.image,
                        audio: options.audio,
                        video: options.video,
                        ..UploadOptions::default()
                    },
                    recipe.unwrap(),
                    expires_at,
                    input,
                    cancel.clone(),
                )
                .await
            },
            Work::Export(options) => self.export_archive(id, options, cancel.clone()).await,
            Work::Mp4Export(work) => self.run_mp4_export(id, work, cancel.clone()).await,
            Work::Import {
                ..
            } => self.import_archive(id, cancel.clone()).await,
        };
        let outcome = if self.0.shutdown.load(Ordering::Acquire)
            && result.is_err()
            && !cancel.load(Ordering::Acquire)
        {
            self.requeue_interrupted(id).await
        } else {
            self.finish_task(id, result).await
        };
        if let Err(error) = outcome {
            tracing::error!(task_id=%id, %error, "cannot record task outcome");
        }
        if let Err(error) = self.clean_task_files(id, false).await {
            tracing::warn!(task_id=%id, %error, "task staging cleanup failed");
        }
        let mut active = self.0.cancellations.lock().unwrap();
        if active.get(&id).is_some_and(|current| Arc::ptr_eq(current, &cancel)) {
            active.remove(&id);
        }
    }

    // Put a task stopped by shutdown back into the queue, so the next start does not count it as a crash.
    async fn requeue_interrupted(&self, id: Uuid) -> Result<(), ServiceError> {
        let _mutation = self.0.mutations.lock().await;
        let Some(mut task) = self.get_task(id).await? else {
            return Ok(());
        };
        if task.status != TaskStatus::Running {
            return Ok(());
        }
        task.status = TaskStatus::Queued;
        task.stage = "queued".into();
        self.save_task(&mut task).await
    }

    /// Run one queued task in the current async task and return its final state.
    /// Use it with `new_without_workers` to run a single task without processing the rest of the queue.
    /// It returns `Conflict` when the task is not queued, for example when a worker has already claimed it.
    pub async fn run_task(&self, id: Uuid) -> Result<Task, ServiceError> {
        if self.0.shutdown.load(Ordering::Acquire) {
            return Err(ServiceError::Busy);
        }
        let Some((task, work, cancel)) = self.claim_task(Some(id)).await? else {
            return Err(match self.get_task(id).await? {
                Some(_) => ServiceError::Conflict("the task is not queued".into()),
                None => ServiceError::NotFound,
            });
        };
        self.run_claimed(task, work, cancel).await;
        self.get_task(id).await?.ok_or(ServiceError::NotFound)
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
            if let Err(error) = service.expire_mp4_artifacts().await {
                tracing::warn!(%error, "MP4 artifact cleanup failed");
            }
            if let Err(error) = service.clear_released_files().await {
                tracing::warn!(%error, "released file cleanup failed");
            }
            // Keep a full scan for files left by interrupted work or older storage methods.
            if last_scan.elapsed() >= FULL_SCAN_INTERVAL {
                match service.clear_untracked_files().await {
                    Ok(true) => last_scan = tokio::time::Instant::now(),
                    Ok(false) => (),
                    Err(error) => tracing::warn!(%error, "stored file cleanup failed"),
                }
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
                if task.status == TaskStatus::Cancelling {
                    task.status = TaskStatus::Cancelled;
                    task.stage = status_name(task.status).into();
                    self.save_task(&mut task).await?;
                } else {
                    let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
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
                || (name == "snapshot"
                    && task.kind == "mp4_export"
                    && task.status != TaskStatus::Succeeded)
                || (name == "export.mp4"
                    && task.kind == "mp4_export"
                    && task.status == TaskStatus::Succeeded)
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

    pub(super) async fn expire_tasks(&self) -> Result<(), ServiceError> {
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
             < ? AND NOT EXISTS(SELECT 1 FROM mp4_artifacts a WHERE a.task_id=tasks.id AND \
             a.expires_at>?) RETURNING id",
        )
        .bind(cutoff)
        .bind(Utc::now().timestamp_millis())
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
        mut options: UploadOptions,
        recipe: ProcessingRecipe,
        source_expiry: Option<chrono::DateTime<Utc>>,
        staged: StagedInput,
        cancel: Arc<AtomicBool>,
    ) -> Result<serde_json::Value, ServiceError> {
        #[cfg(not(feature = "image-convert"))]
        let _ = &recipe;
        let input = self.work_directory(id).join("input");
        if options.automatic() {
            self.processing_progress(id, "detecting", 0, 0).await?;
            let kind = super::classification::detect(self, &input, &options, &cancel).await?;
            options.kind = match kind {
                MediaKind::Image if options.enable_convert_to_image => kind,
                MediaKind::Audio if options.enable_convert_to_audio => kind,
                MediaKind::Video if options.enable_convert_to_video => kind,
                _ => MediaKind::Resource,
            };
        }
        let output = self.work_directory(id).join(format!("output-{}", Uuid::new_v4()));
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
            audio: None,
            video: None,
            warnings: Vec::new(),
            expires_at: None,
            single_use: options.retention.single_use,
            consumed_at: None,
            animated: false,
            frame_count: 1,
        };
        let mut prepared = HashMap::new();
        let mut inventory = None;
        let mut mime =
            options.file_type.clone().unwrap_or_else(|| "application/octet-stream".into());
        if options.kind == MediaKind::Image {
            #[cfg(feature = "image-convert")]
            {
                let image_input = input.clone();
                let image_options = options.image.clone();
                let limits = recipe.image_limits.clone();
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
                // `Path::extension` ignores a trailing `/` or `/.`, so only strip it when the name really ends with it.
                let stem = std::path::Path::new(&name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(|extension| name.strip_suffix(extension)?.strip_suffix('.'))
                    .unwrap_or(&name);
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
                        processing_method: variant.processing_method,
                        name:              variant.spec.name.clone(),
                        multiplier:        variant.multiplier,
                        format:            variant.format.clone(),
                        width:             variant.width,
                        height:            variant.height,
                        animated:          variant.animated,
                        file:              file.metadata.clone(),
                        content_path:      content_path(
                            id,
                            &variant.spec.name,
                            variant.multiplier,
                            &variant.format,
                        ),
                        recipe:            Some(variant.spec),
                    });
                    prepared.insert(file.metadata.id, file);
                }
            }
            #[cfg(not(feature = "image-convert"))]
            return Err(ServiceError::Unsupported("image processing is disabled".into()));
        } else if matches!(options.kind, MediaKind::Audio | MediaKind::Video) {
            #[cfg(feature = "av-convert")]
            {
                let processed = super::av_processor::process(
                    self, id, &input, &output, &options, &recipe, &cancel,
                )
                .await?;
                mime = processed.original_mime;
                media.audio = processed.audio;
                media.video = processed.video;
                media.warnings = processed.warnings;
                inventory = processed.inventory;
                prepared.extend(processed.prepared);
            }
            #[cfg(not(feature = "av-convert"))]
            return Err(ServiceError::Unsupported(
                "audio and video processing are disabled".into(),
            ));
        } else if options.file_type.is_none()
            && let Some(detected) = crate::functions::detect_file_type_by_path(&input).await
        {
            mime = detected.to_string();
        }
        let save_original = match options.kind {
            MediaKind::Resource => true,
            MediaKind::Image => options.image.save_original,
            MediaKind::Audio => options.audio.save_original,
            MediaKind::Video => options.video.save_original,
        };
        if save_original {
            let size = match staged.file_size {
                Some(size) => size,
                None => fs::metadata(&input).await?.len(),
            };
            let file = PreparedFile {
                path:     input,
                metadata: MediaFile {
                    id:        Uuid::new_v4(),
                    sha256:    staged.hash,
                    file_size: size.to_string(),
                    file_type: mime,
                    file_name: name,
                },
            };
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
        self.publish_media_tx(&mut tx, &mut media, inventory.as_mut(), &prepared, &mut guards)
            .await?;
        let result = serde_json::to_value(&media)?;
        Self::complete_task_tx(&mut tx, id, &result).await?;
        tx.commit().await?;
        drop(guards);
        Ok(result)
    }
}

pub(super) fn validate_idempotency_key(key: Option<&str>) -> Result<(), ServiceError> {
    if key.is_some_and(|key| {
        key.is_empty() || key.len() > 128 || key.bytes().any(|byte| !(0x20..=0x7E).contains(&byte))
    }) {
        return Err(ServiceError::Invalid(
            "Idempotency-Key must contain 1 to 128 printable bytes".into(),
        ));
    }
    Ok(())
}

fn process_fingerprint(source: Uuid, options: &ProcessOptions) -> Result<String, ServiceError> {
    let request = serde_json::json!({"Process": {"source": source, "options": options}});
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&request)?)))
}

fn request_fingerprint(work: &Work) -> Result<String, ServiceError> {
    if let Work::Mp4Export(work) = work {
        return mp4_fingerprint(work.media_id, &work.options);
    }
    if let Work::Process {
        source,
        options,
        ..
    } = work
    {
        return process_fingerprint(*source, options);
    }
    let mut request = serde_json::to_value(work)?;
    // The saved input size is a storage detail, so old and new requests use the same identity.
    if let Some(serde_json::Value::Object(upload)) = request.get_mut("Upload") {
        upload.remove("file_size");
        upload.remove("recipe");
    }
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&request)?)))
}

pub(super) fn mp4_fingerprint(
    media: Uuid,
    options: &super::Mp4ExportOptions,
) -> Result<String, ServiceError> {
    let request = serde_json::json!({"Mp4Export": {"media_id": media, "options": options}});
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&request)?)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_request_identity_ignores_default_additions_and_recipe_snapshots() {
        let options = serde_json::json!({
            "kind": "resource", "file_name": null, "file_type": null,
            "retention": {"expires_in_seconds": null, "single_use": false},
            "image": {"save_original": true, "variants": [{
                "name": "default", "max_width": null, "max_height": null,
                "crop": null, "multipliers": [1, 2, 3]
            }]}
        });
        let legacy = serde_json::json!({"Upload": {"options": options.clone(), "hash": "abc"}});
        let expected = hex::encode(Sha256::digest(serde_json::to_vec(&legacy).unwrap()));
        let mut work: Work = serde_json::from_value(legacy).unwrap();
        assert_eq!(expected, request_fingerprint(&work).unwrap());
        if let Work::Upload {
            recipe,
            input,
            ..
        } = &mut work
        {
            *recipe = Some(ProcessingRecipe::from_config(&super::super::ServiceConfig::default()));
            input.file_size = Some(3);
        }
        assert_eq!(expected, request_fingerprint(&work).unwrap());

        let source = Uuid::new_v4();
        let process = serde_json::json!({"Process": {"source": source, "options": {"kind": "image", "image": options["image"]}}});
        let expected = hex::encode(Sha256::digest(serde_json::to_vec(&process).unwrap()));
        assert_eq!(expected, process_fingerprint(source, &ProcessOptions::default()).unwrap());
        let mut changed = ProcessOptions::default();
        changed.image.processing_mode = super::super::ProcessingMode::Trust;
        assert_ne!(expected, process_fingerprint(source, &changed).unwrap());
    }
}
