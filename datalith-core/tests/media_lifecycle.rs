#![cfg(feature = "av-convert")]

use std::{path::Path, time::Duration};

use chrono::{DateTime, Utc};
use datalith_core::{
    Datalith, DatalithService, HlsAsset, Media, MediaKind, Mp4ExportOptions, Mp4ExportResult,
    ProcessingMode, Retention, ServiceConfig, ServiceError, Task, TaskStatus, UploadOptions, Uuid,
    VideoOptions, VideoVariantSpec,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
};

fn config() -> ServiceConfig {
    let mut config = ServiceConfig::default();
    config.av.ffmpeg =
        std::env::var_os("DATALITH_FFMPEG").unwrap_or_else(|| "ffmpeg".into()).into();
    config.av.ffprobe =
        std::env::var_os("DATALITH_FFPROBE").unwrap_or_else(|| "ffprobe".into()).into();
    config.av.encoder_threads = 2;
    config.workers = 1;
    config.playback_session_seconds = 60;
    config.task_retention_seconds = 60;
    config.mp4_export_retention_seconds = 2;
    config
}

async fn service(environment: &Path, config: ServiceConfig) -> DatalithService {
    DatalithService::new(Datalith::new(environment).await.unwrap(), config).await.unwrap()
}

async fn finished(service: &DatalithService, id: Uuid) -> Task {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let task = service.get_task(id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
                return task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn upload(service: &DatalithService, source: &Path, single_use: bool) -> Media {
    let task = service
        .submit_upload(
            fs::File::open(source).await.unwrap(),
            UploadOptions {
                kind: MediaKind::Video,
                retention: Retention {
                    single_use,
                    ..Retention::default()
                },
                video: VideoOptions {
                    processing_mode: ProcessingMode::Trust,
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
    serde_json::from_value(finished(service, task.id).await.result.unwrap()).unwrap()
}

async fn export(service: &DatalithService, media: &Media) -> (Task, Mp4ExportResult) {
    let task = service
        .submit_mp4_export(
            media.id,
            Mp4ExportOptions {
                variant: media.video.as_ref().unwrap().variants[0].id.clone()
            },
            None,
            None,
        )
        .await
        .unwrap();
    let task = finished(service, task.id).await;
    let result = serde_json::from_value(task.result.clone().unwrap()).unwrap();
    (task, result)
}

async fn artifact_bytes(service: &DatalithService, id: Uuid) -> Vec<u8> {
    let mut content = service.open_artifact(id).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    bytes
}

async fn range_bytes(service: &DatalithService, media: &Media, token: &str) -> [u8; 32] {
    let track = &media.video.as_ref().unwrap().variants[0].id;
    let mut content =
        service.open_hls_content(media.id, track, HlsAsset::Segment(0), Some(token)).await.unwrap();
    assert!(content.repeatable);
    assert!(content.single_use);
    content.file.seek(std::io::SeekFrom::Start(16)).await.unwrap();
    let mut bytes = [0; 32];
    content.file.read_exact(&mut bytes).await.unwrap();
    bytes
}

async fn wait_until(deadline: DateTime<Utc>) {
    let remaining = (deadline - Utc::now()).num_milliseconds().max(0) as u64;
    tokio::time::sleep(Duration::from_millis(remaining + 75)).await;
}

#[tokio::test]
async fn sessions_and_artifact_deadlines_survive_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let environment = directory.path().join("store");
    let source = directory.path().join("source.mp4");
    let short_config = config();
    let output = Command::new(&short_config.av.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:size=256x144:rate=12:duration=1",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-profile:v",
            "high",
            "-level:v",
            "1.2",
            "-refs",
            "1",
            "-bf",
            "0",
            "-g",
            "24",
            "-flags",
            "+cgop",
            "-x264-params",
            "open-gop=0:scenecut=0",
            "-movflags",
            "+faststart",
        ])
        .arg(&source)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let first = service(&environment, short_config.clone()).await;
    let ordinary = upload(&first, &source, false).await;
    let single_use = upload(&first, &source, true).await;
    let claim_key = "lifecycle-playback".to_owned();
    let session =
        first.claim_playback_session(single_use.id, Some(claim_key.clone())).await.unwrap();
    let original_range = range_bytes(&first, &single_use, &session.token).await;
    let (short_task, short_artifact) = export(&first, &ordinary).await;
    let short_bytes = artifact_bytes(&first, short_task.id).await;
    assert!(short_bytes.len() > 32);
    first.close().await.unwrap();
    drop(first);

    // Restart while the artifact is still live, with the same persisted playback session.
    let reopened = service(&environment, short_config.clone()).await;
    let replay =
        reopened.claim_playback_session(single_use.id, Some(claim_key.clone())).await.unwrap();
    assert!(session.token == replay.token, "playback credential changed after restart");
    assert_eq!(session.expires_at, replay.expires_at);
    assert_eq!(original_range, range_bytes(&reopened, &single_use, &session.token).await);
    assert_eq!(short_bytes, artifact_bytes(&reopened, short_task.id).await);
    reopened.close().await.unwrap();
    drop(reopened);

    wait_until(short_artifact.expires_at).await;
    let expired = service(&environment, short_config).await;
    assert!(expired.get_task(short_task.id).await.unwrap().is_some());
    assert!(matches!(expired.open_artifact(short_task.id).await, Err(ServiceError::NotFound)));
    let short_path =
        environment.join("datalith.tasks").join(short_task.id.to_string()).join("export.mp4");
    assert!(!fs::try_exists(&short_path).await.unwrap());
    expired.close().await.unwrap();
    drop(expired);

    let mut long_config = config();
    long_config.task_retention_seconds = 1;
    long_config.mp4_export_retention_seconds = 4;
    let long_lived = service(&environment, long_config.clone()).await;
    let (long_task, long_artifact) = export(&long_lived, &ordinary).await;
    let long_bytes = artifact_bytes(&long_lived, long_task.id).await;
    long_lived.close().await.unwrap();
    drop(long_lived);

    // A finished export remains readable when only its task's usual retention has elapsed.
    wait_until(long_task.updated_at + chrono::Duration::seconds(1)).await;
    let retained = service(&environment, long_config.clone()).await;
    assert!(Utc::now() < long_artifact.expires_at);
    assert_eq!(
        TaskStatus::Succeeded,
        retained.get_task(long_task.id).await.unwrap().unwrap().status
    );
    assert_eq!(long_bytes, artifact_bytes(&retained, long_task.id).await);
    retained.close().await.unwrap();
    drop(retained);

    wait_until(long_artifact.expires_at).await;
    let cleaned = service(&environment, long_config).await;
    assert!(cleaned.get_task(long_task.id).await.unwrap().is_none());
    assert!(matches!(cleaned.open_artifact(long_task.id).await, Err(ServiceError::NotFound)));
    let long_path =
        environment.join("datalith.tasks").join(long_task.id.to_string()).join("export.mp4");
    assert!(!fs::try_exists(&long_path).await.unwrap());
    assert_eq!(original_range, range_bytes(&cleaned, &single_use, &session.token).await);
    let replay = cleaned.claim_playback_session(single_use.id, Some(claim_key)).await.unwrap();
    assert!(session.token == replay.token, "playback credential changed after cleanup");
    assert_eq!(session.expires_at, replay.expires_at);
    cleaned.close().await.unwrap();
}
