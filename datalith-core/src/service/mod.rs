mod archive;
#[cfg(feature = "image-convert")]
mod image_processor;
pub(crate) mod migration;
mod store;
mod tasks;
mod types;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::{DateTime, Utc};
use tokio::{
    fs::File,
    sync::{Mutex as AsyncMutex, Notify, OwnedRwLockReadGuard, RwLock},
    task::JoinHandle,
};
pub use types::*;
use uuid::Uuid;

use crate::{Datalith, guard::OpenGuard};

/// A media service with durable background tasks, built on a `Datalith` store.
#[derive(Clone)]
pub struct DatalithService(pub(super) Arc<ServiceInner>);

/// An opened file that is ready to be read.
/// Keep it until the read is done, because it stops the file from being removed.
pub struct Content {
    /// The opened file.
    pub file:                   File,
    /// The stored file metadata.
    pub metadata:               MediaFile,
    /// The time when the content was created.
    pub created_at:             DateTime<Utc>,
    /// Whether the content can be downloaded only once.
    pub single_use:             bool,
    /// Whether the content expires, so it must not be cached.
    pub temporary:              bool,
    pub(super) _file_guard:     Option<OpenGuard>,
    pub(super) _artifact_guard: Option<OwnedRwLockReadGuard<()>>,
}

pub(super) struct ServiceInner {
    datalith:       Datalith,
    config:         ServiceConfig,
    wakeup:         Arc<Notify>,
    stopping:       Arc<Notify>,
    shutdown:       AtomicBool,
    // Set when a stored file loses its last reference, so that the maintenance task scans the file directory.
    released_files: AtomicBool,
    cancellations:  Mutex<HashMap<Uuid, Arc<AtomicBool>>>,
    workers:        AsyncMutex<Vec<JoinHandle<()>>>,
    writes:         RwLock<()>,
    mutations:      AsyncMutex<()>,
    artifacts:      Arc<RwLock<()>>,
}

impl DatalithService {
    /// Start the service, recover unfinished tasks, and run the background workers.
    /// Only one service can use a store at a time.
    pub async fn new(datalith: Datalith, config: ServiceConfig) -> Result<Self, ServiceError> {
        if config.workers == 0
            || config.workers > 64
            || config.max_file_size == 0
            || config.task_retention_seconds == 0
        {
            return Err(ServiceError::Invalid("invalid service limits".into()));
        }
        if datalith
            .0
            ._service_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ServiceError::Conflict("a service already owns this database".into()));
        }
        let service = Self(Arc::new(ServiceInner {
            datalith,
            config,
            wakeup: Arc::new(Notify::new()),
            stopping: Arc::new(Notify::new()),
            shutdown: AtomicBool::new(false),
            released_files: AtomicBool::new(false),
            cancellations: Mutex::new(HashMap::new()),
            workers: AsyncMutex::new(Vec::new()),
            writes: RwLock::new(()),
            mutations: AsyncMutex::new(()),
            artifacts: Arc::new(RwLock::new(())),
        }));
        service.recover_tasks().await?;
        service.collect_garbage().await?;
        service.clear_untracked_files().await?;
        store::sync_directory(service.0.datalith.get_environment()).await?;
        let mut workers = service.0.workers.lock().await;
        for _ in 0..service.0.config.workers {
            let worker = Arc::downgrade(&service.0);
            workers.push(tokio::spawn(async move {
                Self::worker(worker).await;
            }));
        }
        let maintenance = Arc::downgrade(&service.0);
        workers.push(tokio::spawn(async move {
            Self::maintenance(maintenance).await;
        }));
        drop(workers);
        Ok(service)
    }

    /// Describe the features and limits of this service as JSON.
    pub fn capabilities(&self) -> serde_json::Value {
        serde_json::json!({
            "api_version": "1", "version": env!("CARGO_PKG_VERSION"),
            "media": {"resource": true, "image": cfg!(feature="image-convert"), "audio": false, "video": false},
            "image": {"engine": "image-convert", "animated_inputs": ["gif", "webp", "apng"], "outputs": ["webp", "png", "jpeg", "gif"], "apng_requires_ffmpeg": true, "apng_timing_precision_ms": 10, "limits": self.0.config.image_limits},
            "max_file_size": self.0.config.max_file_size.to_string(),
            "task_retention_seconds": self.0.config.task_retention_seconds,
            "task_notifications": ["polling"], "cancellation": "safe_points",
            "archive_version": 1, "export_pauses_writes": true,
            "future_profiles": {"audio": ["m4a:aac-lc", "mp3:lame"], "video": ["mp4:h264-x264+aac-lc"]}
        })
    }

    /// Stop the background workers and close the store.
    pub async fn close(&self) -> Result<(), ServiceError> {
        self.0.shutdown.store(true, Ordering::Release);
        self.0.wakeup.notify_waiters();
        self.0.stopping.notify_waiters();
        let mut workers = self.0.workers.lock().await;
        let mut failure = None;
        while let Some(worker) = workers.last_mut() {
            if let Err(error) = worker.await {
                failure.get_or_insert_with(|| ServiceError::Internal(error.to_string()));
            }
            workers.pop();
        }
        self.0.datalith.clone().close().await;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        self.datalith.0._service_active.store(false, Ordering::Release);
    }
}
