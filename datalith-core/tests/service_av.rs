#![cfg(feature = "av-convert")]

use std::{path::Path, time::Duration};

use datalith_core::{
    AudioOptions, ContentRequest, Datalith, DatalithService, Media, MediaKind, ProcessingMethod,
    ProcessingMode, ServiceConfig, Task, TaskStatus, UploadOptions, Uuid, VideoOptions,
    VideoVariantSpec,
};
use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncReadExt, process::Command};

fn ffmpeg() -> std::ffi::OsString {
    std::env::var_os("DATALITH_FFMPEG").unwrap_or_else(|| "ffmpeg".into())
}
fn ffprobe() -> std::ffi::OsString {
    std::env::var_os("DATALITH_FFPROBE").unwrap_or_else(|| "ffprobe".into())
}

async fn fixture(path: &Path, arguments: &[&str]) {
    let output = Command::new(ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(arguments)
        .arg(path)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

async fn finished(service: &DatalithService, id: Uuid) -> Task {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let task = service.get_task(id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

async fn upload(service: &DatalithService, path: &Path, options: UploadOptions) -> Media {
    let task =
        service.submit_upload(fs::File::open(path).await.unwrap(), options, None).await.unwrap();
    let task = finished(service, task.id).await;
    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
    serde_json::from_value(task.result.unwrap()).unwrap()
}

async fn service(path: &Path) -> DatalithService {
    let mut config = ServiceConfig::default();
    config.av.ffmpeg = ffmpeg().into();
    config.av.ffprobe = ffprobe().into();
    config.av.encoder_threads = 2;
    let service = DatalithService::new(Datalith::new(path).await.unwrap(), config).await.unwrap();
    assert_eq!(true, service.capabilities()["media"]["audio"]);
    service
}

#[tokio::test]
async fn audio_encodes_aac_preserves_integer_samples_and_reports_float_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let integer = directory.path().join("integer.wav");
    fixture(&integer, &[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=44100:duration=1",
        "-c:a",
        "pcm_s16le",
    ])
    .await;
    let service = service(&directory.path().join("store")).await;
    let media = upload(&service, &integer, UploadOptions {
        kind: MediaKind::Audio,
        audio: AudioOptions {
            preserve_lossless: true,
            ..AudioOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    assert!(media.original.is_none());
    let audio = media.audio.unwrap();
    assert_eq!(2, audio.variants.len());
    let aac = audio.variants.iter().find(|variant| variant.codec == "aac").unwrap();
    assert_eq!(48_000, aac.sample_rate);
    assert_eq!(1, aac.channels);
    let flac = audio.variants.iter().find(|variant| variant.codec == "flac").unwrap();
    assert_eq!(44_100, flac.sample_rate);
    assert_eq!(Some(16), flac.bits_per_sample);
    let mut content = service
        .open_content(
            media.id,
            ContentRequest {
                format: Some("flac".into()),
                ..ContentRequest::default()
            },
            false,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    drop(content);
    let flac_path = directory.path().join("result.flac");
    fs::write(&flac_path, bytes).await.unwrap();
    let source_pcm = Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(&integer)
        .args(["-f", "s16le", "pipe:1"])
        .output()
        .await
        .unwrap();
    let result_pcm = Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(&flac_path)
        .args(["-f", "s16le", "pipe:1"])
        .output()
        .await
        .unwrap();
    assert!(source_pcm.status.success());
    assert!(result_pcm.status.success());
    assert_eq!(source_pcm.stdout, result_pcm.stdout);

    let floating = directory.path().join("floating.wav");
    fixture(&floating, &[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=1",
        "-c:a",
        "pcm_f32le",
    ])
    .await;
    let media = upload(&service, &floating, UploadOptions {
        kind: MediaKind::Audio,
        audio: AudioOptions {
            preserve_lossless: true,
            ..AudioOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    assert_eq!("lossless_not_preserved", media.warnings[0].code);
    assert_eq!(1, media.audio.unwrap().variants.len());
    service.close().await.unwrap();
}

#[tokio::test]
async fn compliant_aac_trust_reuses_the_file_without_retaining_an_original() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.m4a");
    fixture(&source, &[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=1",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-movflags",
        "+faststart",
    ])
    .await;
    let source_bytes = fs::read(&source).await.unwrap();
    let service = service(&directory.path().join("store")).await;
    let media = upload(&service, &source, UploadOptions {
        kind: MediaKind::Audio,
        audio: AudioOptions {
            processing_mode: ProcessingMode::Trust,
            ..AudioOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    assert!(media.original.is_none());
    let audio = media.audio.unwrap();
    assert_eq!(1, audio.variants.len());
    assert_eq!(ProcessingMethod::Copied, audio.variants[0].processing_method);
    assert_eq!(
        hex::encode(Sha256::digest(&source_bytes)),
        audio.variants[0].file.as_ref().unwrap().sha256
    );
    let mut content =
        service.open_content(media.id, ContentRequest::default(), false).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    drop(content);
    assert_eq!(source_bytes, bytes);
    service.close().await.unwrap();
}

#[tokio::test]
async fn flac_preserves_the_lowest_bits_of_integer_32_bit_samples() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let samples = (0i32..48_000)
        .flat_map(|index| ((index % 2000 - 1000) * 1024 + 1).to_le_bytes())
        .collect::<Vec<_>>();
    let mut wave = Vec::new();
    wave.extend(b"RIFF");
    wave.extend((36 + samples.len() as u32).to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(16u32.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(48_000u32.to_le_bytes());
    wave.extend(192_000u32.to_le_bytes());
    wave.extend(4u16.to_le_bytes());
    wave.extend(32u16.to_le_bytes());
    wave.extend(b"data");
    wave.extend((samples.len() as u32).to_le_bytes());
    wave.extend(&samples);
    fs::write(&source, wave).await.unwrap();
    let service = service(&directory.path().join("store")).await;
    let media = upload(&service, &source, UploadOptions {
        kind: MediaKind::Audio,
        audio: AudioOptions {
            preserve_lossless: true,
            ..AudioOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    let audio = media.audio.unwrap();
    assert_eq!(
        Some(32),
        audio.variants.iter().find(|variant| variant.codec == "flac").unwrap().bits_per_sample
    );
    let mut content = service
        .open_content(
            media.id,
            ContentRequest {
                format: Some("flac".into()),
                ..ContentRequest::default()
            },
            false,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    drop(content);
    let path = directory.path().join("output.flac");
    fs::write(&path, bytes).await.unwrap();
    let decoded = Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s32le", "pipe:1"])
        .output()
        .await
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    assert_eq!(samples, decoded.stdout);
    service.close().await.unwrap();
}

#[tokio::test]
async fn flac_preserves_pcm_eight_bit_samples_and_companded_pcm_stays_aac() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.wav");
    let samples = (0u8..=255).cycle().take(48_128).collect::<Vec<_>>();
    let mut wave = Vec::new();
    wave.extend(b"RIFF");
    wave.extend((36 + samples.len() as u32).to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(16u32.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(48_000u32.to_le_bytes());
    wave.extend(48_000u32.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(8u16.to_le_bytes());
    wave.extend(b"data");
    wave.extend((samples.len() as u32).to_le_bytes());
    wave.extend(&samples);
    fs::write(&source, wave).await.unwrap();
    let service = service(&directory.path().join("store")).await;
    let options = UploadOptions {
        kind: MediaKind::Audio,
        audio: AudioOptions {
            preserve_lossless: true,
            ..AudioOptions::default()
        },
        ..UploadOptions::default()
    };
    let media = upload(&service, &source, options.clone()).await;
    assert!(media.warnings.is_empty());
    assert_eq!(
        Some(16),
        media
            .audio
            .as_ref()
            .unwrap()
            .variants
            .iter()
            .find(|variant| variant.codec == "flac")
            .unwrap()
            .bits_per_sample
    );
    let mut content = service
        .open_content(
            media.id,
            ContentRequest {
                format: Some("flac".into()),
                ..ContentRequest::default()
            },
            false,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    drop(content);
    let path = directory.path().join("output.flac");
    fs::write(&path, bytes).await.unwrap();
    let decoded = Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "u8", "pipe:1"])
        .output()
        .await
        .unwrap();
    assert!(decoded.status.success(), "{}", String::from_utf8_lossy(&decoded.stderr));
    assert_eq!(samples, decoded.stdout);
    let lossy = directory.path().join("companded.wav");
    fixture(&lossy, &["-f", "lavfi", "-i", "sine=sample_rate=8000:duration=1", "-c:a", "pcm_alaw"])
        .await;
    let media = upload(&service, &lossy, options).await;
    assert_eq!(1, media.audio.as_ref().unwrap().variants.len());
    assert_eq!("aac", media.audio.unwrap().variants[0].codec);
    assert!(media.warnings.is_empty());
    service.close().await.unwrap();
}

async fn stored_track(
    pool: &sqlx::SqlitePool,
    directory: &Path,
    track: &serde_json::Value,
) -> Vec<u8> {
    let mut result = Vec::new();
    let files = std::iter::once(&track["initialization"])
        .chain(track["segments"].as_array().unwrap().iter().map(|segment| &segment["file"]));
    for file in files {
        let id: Uuid = file["id"].as_str().unwrap().parse().unwrap();
        let storage: Uuid = sqlx::query_scalar(
            "SELECT COALESCE(b.storage_id,f.id) FROM files f LEFT JOIN blob_files b ON \
             b.file_id=f.id WHERE f.id=?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
        result.extend(
            fs::read(
                directory
                    .join(datalith_core::PATH_FILE_DIRECTORY)
                    .join(format!("{:x}", storage.as_u128())),
            )
            .await
            .unwrap(),
        );
    }
    result
}

async fn packet_hashes(path: &Path, stream: &str) -> Vec<String> {
    let result = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=data_hash",
            "-of",
            "compact=p=0:nk=1",
        ])
        .arg(path)
        .output()
        .await
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            line.find("SHA256:").map(|offset| line[offset + 7..offset + 71].to_owned())
        })
        .collect()
}

#[tokio::test]
async fn video_creates_only_hls_assets_and_collapses_effective_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    fixture(&source, &[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=26:duration=2",
        "-f",
        "lavfi",
        "-i",
        "sine=sample_rate=48000:duration=2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-shortest",
    ])
    .await;
    let service = service(&directory.path().join("store")).await;
    let media = upload(&service, &source, UploadOptions {
        kind: MediaKind::Video,
        video: VideoOptions {
            variants: vec![
                VideoVariantSpec {
                    resolution: 1080, fps: 60
                },
                VideoVariantSpec {
                    resolution: 720, fps: 25
                },
            ],
            ..VideoOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    assert!(media.original.is_none());
    let video = media.video.unwrap();
    assert_eq!(1, video.variants.len());
    assert_eq!(
        (144, 25, 256, 144),
        (
            video.variants[0].resolution,
            video.variants[0].fps,
            video.variants[0].width,
            video.variants[0].height
        )
    );
    assert_eq!(vec!["aac_low"], video.variants[0].audio);
    assert_eq!(1, video.audio.len());
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("store").join(datalith_core::PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let inventory: String = sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
        .bind(media.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_str(&inventory).unwrap();
    assert_eq!(2, inventory["tracks"].as_array().unwrap().len());
    assert!(
        inventory["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|track| !track["segments"].as_array().unwrap().is_empty())
    );
    let complete_mp4: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_files WHERE media_id=? AND role='original'")
            .bind(media.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(0, complete_mp4);
    pool.close().await;
    service.close().await.unwrap();
}

#[tokio::test]
async fn video_trust_preserves_compliant_h264_and_mixed_rates_share_a_time_origin() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    fixture(&source, &[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=256x144:rate=30000/1001:duration=13",
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-profile:v",
        "high",
        "-level:v",
        "1.2",
        "-refs",
        "4",
        "-flags",
        "+cgop",
        "-x264-params",
        "open-gop=0:scenecut=0",
        "-forced-idr",
        "1",
        "-force_key_frames",
        "expr:gte(t,n_forced*2)",
        "-movflags",
        "+faststart",
    ])
    .await;
    let service = service(&directory.path().join("store")).await;
    let media = upload(&service, &source, UploadOptions {
        kind: MediaKind::Video,
        video: VideoOptions {
            processing_mode: ProcessingMode::Trust,
            variants: vec![
                VideoVariantSpec {
                    resolution: 144, fps: 24
                },
                VideoVariantSpec {
                    resolution: 144, fps: 30
                },
            ],
            ..VideoOptions::default()
        },
        ..UploadOptions::default()
    })
    .await;
    let video = media.video.unwrap();
    assert_eq!(ProcessingMethod::Transcoded, video.variants[0].processing_method);
    assert_eq!(ProcessingMethod::Remuxed, video.variants[1].processing_method);
    assert_eq!(
        (30_000, 1001),
        (video.variants[1].frame_rate.numerator, video.variants[1].frame_rate.denominator)
    );
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("store").join(datalith_core::PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let inventory: String = sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
        .bind(media.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_str(&inventory).unwrap();
    let tracks = inventory["tracks"].as_array().unwrap();
    assert_eq!(2, tracks.len());
    assert_eq!(tracks[0]["segments"][0]["start"], tracks[1]["segments"][0]["start"]);
    for track in tracks {
        let first = track["segments"][0]["start"].as_i64().unwrap();
        assert_eq!(120_000, track["timescale"]);
        for (index, segment) in track["segments"].as_array().unwrap().iter().enumerate() {
            let actual = segment["start"].as_i64().unwrap() - first;
            let target = index as i64 * 720_000;
            assert!(actual >= target && actual - target < 5001, "{actual} {target}");
        }
    }
    let copied = tracks.iter().find(|track| track["id"] == "144p30").unwrap();
    let output = directory.path().join("copied.mp4");
    fs::write(&output, stored_track(&pool, &directory.path().join("store"), copied).await)
        .await
        .unwrap();
    assert_eq!(packet_hashes(&source, "v:0").await, packet_hashes(&output, "v:0").await);
    pool.close().await;
    service.close().await.unwrap();
}

#[tokio::test]
async fn audio_offsets_keep_the_full_effective_presentation_window() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(&directory.path().join("store")).await;
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(directory.path().join("store").join(datalith_core::PATH_DB_FILE)),
    )
    .await
    .unwrap();
    for offset in ["0.15", "-0.15"] {
        let source = directory.path().join(format!("source-{offset}.mp4"));
        fixture(&source, &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=256x144:rate=30:duration=3",
            "-itsoffset",
            offset,
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=44100:duration=3",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-profile:v",
            "high",
            "-level:v",
            "1.2",
            "-refs",
            "4",
            "-flags",
            "+cgop",
            "-x264-params",
            "open-gop=0:scenecut=0",
            "-forced-idr",
            "1",
            "-force_key_frames",
            "expr:gte(t,n_forced*2)",
            "-c:a",
            "flac",
            "-sample_fmt",
            "s16",
            "-strict",
            "experimental",
            "-copyts",
            "-output_ts_offset",
            "5",
        ])
        .await;
        let media = upload(&service, &source, UploadOptions {
            kind: MediaKind::Video,
            video: VideoOptions {
                processing_mode: ProcessingMode::Trust,
                preserve_lossless: true,
                variants: vec![VideoVariantSpec {
                    resolution: 1080, fps: 30
                }],
                ..VideoOptions::default()
            },
            ..UploadOptions::default()
        })
        .await;
        let video = media.video.as_ref().unwrap();
        let leading_hold = video.variants[0].leading_hold_seconds;
        assert_eq!(ProcessingMethod::Remuxed, video.variants[0].processing_method);
        assert!((video.duration_seconds - 3.15).abs() < 0.002, "{}", video.duration_seconds);
        let inventory: String =
            sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
                .bind(media.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let inventory: serde_json::Value = serde_json::from_str(&inventory).unwrap();
        let tracks = inventory["tracks"].as_array().unwrap();
        let video = tracks.iter().find(|track| track["id"] == "144p30").unwrap();
        let audio = tracks.iter().find(|track| track["id"] == "aac_low").unwrap();
        let start = audio["presentation_start"].as_i64().unwrap() as f64
            / audio["timescale"].as_u64().unwrap() as f64
            - video["presentation_start"].as_i64().unwrap() as f64
                / video["timescale"].as_u64().unwrap() as f64
            - leading_hold;
        assert!((start - offset.parse::<f64>().unwrap()).abs() < 0.001, "{offset} {start}");
        let copied_path = directory.path().join(format!("copied-{offset}.mp4"));
        fs::write(&copied_path, stored_track(&pool, &directory.path().join("store"), video).await)
            .await
            .unwrap();
        assert_eq!(packet_hashes(&source, "v:0").await, packet_hashes(&copied_path, "v:0").await);
    }
    pool.close().await;
    service.close().await.unwrap();
}

#[tokio::test]
async fn unavailable_tools_leave_resource_storage_working() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = ServiceConfig::default();
    config.av.ffmpeg = directory.path().join("missing-ffmpeg");
    let service =
        DatalithService::new(Datalith::new(directory.path()).await.unwrap(), config).await.unwrap();
    assert_eq!(false, service.capabilities()["media"]["audio"]);
    assert_eq!(false, service.capabilities()["media"]["video"]);
    let task = service
        .submit_upload(b"resource".as_slice(), UploadOptions::default(), None)
        .await
        .unwrap();
    assert_eq!(TaskStatus::Succeeded, finished(&service, task.id).await.status);
    service.close().await.unwrap();
}
