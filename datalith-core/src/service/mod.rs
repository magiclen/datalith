mod archive;
#[cfg(feature = "av-convert")]
mod av_processor;
mod hls;
#[cfg(feature = "image-convert")]
mod image_processor;
pub(crate) mod migration;
mod mp4_export;
#[cfg(feature = "openapi")]
mod openapi;
mod sessions;
mod store;
#[cfg(feature = "image-convert")]
mod svg;
mod tasks;
mod types;

use std::{
    collections::{HashMap, HashSet},
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
    /// Whether this content belongs to single-use media.
    pub single_use:             bool,
    /// Whether this opened content supports repeated reads and byte ranges.
    pub repeatable:             bool,
    /// Whether the content expires, so it must not be cached.
    pub temporary:              bool,
    pub(super) _file_guard:     Option<OpenGuard>,
    pub(super) _artifact_guard: Option<OwnedRwLockReadGuard<()>>,
}

pub(super) struct ServiceInner {
    #[cfg(feature = "av-convert")]
    av:             av_processor::executor::Executor,
    datalith:       Datalith,
    config:         ServiceConfig,
    wakeup:         Arc<Notify>,
    stopping:       Arc<Notify>,
    shutdown:       Arc<AtomicBool>,
    released_files: Mutex<HashSet<Uuid>>,
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
            || config.av.bitrate == 0
            || config.av.max_processes == 0
            || config.av.max_processes > 64
            || config.av.encoder_threads == 0
            || config.playback_session_seconds == 0
            || config.mp4_export_retention_seconds == 0
        {
            return Err(ServiceError::Invalid("invalid service limits".into()));
        }
        #[cfg(feature = "image-convert")]
        tokio::task::spawn_blocking(image_processor::configure_resources)
            .await
            .map_err(|error| ServiceError::Internal(error.to_string()))??;
        let shutdown = Arc::new(AtomicBool::new(false));
        #[cfg(feature = "av-convert")]
        let av = av_processor::executor::Executor::discover(&config.av, shutdown.clone()).await;
        if datalith
            .0
            ._service_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ServiceError::Conflict("a service already owns this database".into()));
        }
        let service = Self(Arc::new(ServiceInner {
            #[cfg(feature = "av-convert")]
            av,
            datalith,
            config,
            wakeup: Arc::new(Notify::new()),
            stopping: Arc::new(Notify::new()),
            shutdown,
            released_files: Mutex::new(HashSet::new()),
            cancellations: Mutex::new(HashMap::new()),
            workers: AsyncMutex::new(Vec::new()),
            writes: RwLock::new(()),
            mutations: AsyncMutex::new(()),
            artifacts: Arc::new(RwLock::new(())),
        }));
        service.recover_tasks().await?;
        service.collect_garbage().await?;
        service.expire_mp4_artifacts().await?;
        service.expire_tasks().await?;
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
        #[cfg(feature = "av-convert")]
        let av = self.0.av.available;
        #[cfg(not(feature = "av-convert"))]
        let av = false;
        #[cfg(feature = "av-convert")]
        let (audio, video) = (av && self.0.av.audio_available, av && self.0.av.video_available);
        #[cfg(not(feature = "av-convert"))]
        let (audio, video) = (av, av);
        #[cfg(feature = "av-convert")]
        let av_status = serde_json::json!({"available":av,"minimum_tool_major":9,"audio_encoder":audio,"video_encoder":video,"flac_encoder":av&&self.0.av.flac_available,"unavailable_reason":self.0.av.reason});
        #[cfg(not(feature = "av-convert"))]
        let av_status = serde_json::json!({"available":false,"minimum_tool_major":9,"audio_encoder":false,"video_encoder":false,"flac_encoder":false,"unavailable_reason":"audio and video processing are disabled"});
        serde_json::json!({
            "api_version": "1", "version": env!("CARGO_PKG_VERSION"),
            "media": {"resource": true, "image": cfg!(feature="image-convert"), "audio": audio, "video": video},
            "image": {"engine": "image-convert", "animated_inputs": ["gif", "webp", "apng"], "outputs": ["webp", "png", "jpeg", "gif"], "apng_requires_ffmpeg": true, "apng_timing_precision_ms": 10, "limits": self.0.config.image_limits,"processing_modes":["transcode","trust"],"save_original_default":true},
            "av":av_status,
            "audio":{"engine":"ffmpeg","profiles":["m4a:aac-lc","flac"],"aac_sample_rate":48000,"aac_bitrates":[256000,128000],"mp3_fallback":false,"save_original_default":false,"processing_modes":["transcode","trust"],"selected_audio_streams":1},
            "video":{"engine":"ffmpeg","codec":"h264","pixel_format":"yuv420p","delivery":"hls-fmp4","requires_variants":true,"resolution_tiers":[144,240,360,432,480,540,576,720,900,1080,1440,2160],"frame_rate_tiers":[10,12,15,20,24,25,30,48,50,60],"bitrate":self.0.config.av.bitrate,"bitrate_unit":"bits_per_second","save_original_default":false,"processing_modes":["transcode","trust"],"mp4_export":av},
            "playback_sessions":{"seconds":self.0.config.playback_session_seconds,"single_use_semantics":"one_claim"},
            "mp4_export_retention_seconds":self.0.config.mp4_export_retention_seconds,
            "max_file_size": self.0.config.max_file_size.to_string(),
            "task_retention_seconds": self.0.config.task_retention_seconds,
            "task_notifications": ["polling"], "cancellation": "safe_points",
            "archive_version": 2, "export_pauses_writes": true
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
