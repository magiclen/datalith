# Datalith Core

Datalith Core is the Rust library behind the Datalith service.
It keeps files on disk, metadata in SQLite, and conversion work in stored background tasks.
Identical stored contents share one copy, and one process owns each data folder.

## Start with the service API

Use `DatalithService` for new applications.
`submit_upload` saves the input and returns a task without waiting for conversion.

```toml
[dependencies]
datalith-core = "0.2"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

```rust,no_run
use datalith_core::{Datalith, DatalithService, Media, ServiceConfig, TaskStatus, UploadOptions};
use std::time::Duration;

# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let store = Datalith::new("data").await?;
let service = DatalithService::new(store, ServiceConfig::default()).await?;
let submitted = service.submit_upload(
    &b"Hello world!"[..],
    UploadOptions { file_name: Some("hello.txt".into()), ..UploadOptions::default() },
    None,
).await?;

loop {
    let task = service.get_task(submitted.id).await?.unwrap();
    if task.status == TaskStatus::Succeeded {
        let media: Media = serde_json::from_value(task.result.unwrap())?;
        println!("Media: {}", media.id);
        break;
    }
    if task.status.is_terminal() {
        return Err(format!("Upload failed: {:?}", task.error).into());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}
service.close().await?;
# Ok(())
# }
```

The input can be any `AsyncRead + Unpin`, including a Tokio file.
An optional idempotency key avoids duplicate tasks when the same request is sent again.
Tasks recover after restart, and failed or cancelled tasks can be retried while their input is kept.

## Enable automatic conversion

```rust
use datalith_core::{UploadOptions, VideoOptions, VideoVariantSpec};

let options = UploadOptions {
    enable_convert_to_image: true,
    enable_convert_to_audio: true,
    enable_convert_to_video: true,
    video: VideoOptions {
        variants: vec![
            VideoVariantSpec { resolution: 720, fps: 30 },
            VideoVariantSpec { resolution: 1080, fps: 60 },
        ],
        ..VideoOptions::default()
    },
    ..UploadOptions::default()
};
```

The service selects a matching enabled type from the file contents.
Other files stay resources.
All switches default to `false`, and settings alone do not enable a type.
Automatic tasks have kind `upload`; the result's `Media.kind` gives the actual type.

Use `ImageOptions` for crops and sizes, `AudioOptions` for audio choices, and `VideoOptions` for video versions.
Images without size limits keep the source size; video needs explicit resolution and fps pairs.
Manual uploads with `MediaKind::Image`, `Audio`, or `Video` remain available, but cannot use automatic switches at the same time.

Images provide WebP and fallback formats, including animated outputs.
Audio provides AAC-LC at 48 kHz, or preserved FLAC with AAC fallback for compatible lossless sources.
Video provides H.264 through HLS with shared audio tracks.
See the [media settings guide](../datalith/README.md#media-settings) for the output rules.

## Read and manage media

| Method or type | Purpose |
| --- | --- |
| `get_media`, `list_media` | Read a Media summary or list items. |
| `open_content`, `ContentRequest` | Open a resource, image output, standalone audio, or saved original. Keep the returned `Content` alive until reading is done. |
| `hls_master`, `hls_track`, `open_hls_content` | Read video playlists and segments. |
| `submit_process` | Create new media from a saved original. |
| `submit_mp4_export` | Copy a stored video version and its best allowed audio into a temporary MP4. |
| `submit_export`, `submit_import`, `open_artifact` | Move media through TAR archives. |
| `cancel_task`, `retry_task` | Cancel work or retry a failed or cancelled task. |
| `delete_media` | Remove media; shared files stay while another item or reader needs them. |
| `close` | Stop workers before closing storage. |

`Retention` sets media expiry and single-use access.
Audio and video use `claim_playback_session` for one timed claim with repeated reads, seeking, and replay.
Pass its token to the session-aware content, HLS, and export methods.
The [service guide](../datalith/README.md#temporary-media) explains these lifetimes.

Images keep the original by default, while audio and video do not.
Set `save_original` to keep a source for later processing.
`ProcessingMode::Trust` reuses content that meets each output's requirements; the default encodes it again.
`ProcessingMethod` reports how an output was made.

`ServiceConfig` controls upload size, workers, image limits, and retention periods.
`AvConfig` controls FFmpeg paths, process count, threads, and the global video bitrate limit.
See the [important settings](../datalith/README.md#service-settings) for defaults and their uses.
Check `capabilities()` before offering conversions that depend on installed tools.

## Build features

| Feature | Purpose |
| --- | --- |
| `magic` | Detect MIME types with libmagic. |
| `image-convert` | Process images with ImageMagick and render SVG with resvg. |
| `av-convert` | Process audio/video with external FFmpeg and ffprobe. |
| `manager` | Provide the older direct API's cleanup scheduler. |
| `openapi` | Add utoipa schemas to request and response types. The HTTP service enables this. |

The first four features are enabled by default.
The task service has its own cleanup and does not need `manager`.
Disable default features for ordinary file storage without native media libraries.
Rust edition 2024 and Rust 1.94 or later are required.
See the [native build guide](../datalith/README.md#api-and-native-builds) and [media tools](../FFMPEG.md).

## Existing applications

The older `DatalithFile`, `DatalithResource`, `DatalithImage`, and direct methods on `Datalith` remain available.
Use `DatalithService` consistently for new applications and avoid mixing its writes with direct storage writes.
Supported older databases are upgraded at startup, preserving IDs and existing outputs.
Back up the whole data folder before upgrading.

Adding the conversion switches means a Rust `UploadOptions` literal that lists every field must add them.
Literals using `..UploadOptions::default()` continue to work.

## License

[MIT](LICENSE)
