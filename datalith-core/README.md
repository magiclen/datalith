# Datalith Core

Datalith Core is the Rust library used by the Datalith service.
It stores file contents on disk and keeps metadata and tasks in SQLite.
Files with the same content share one stored copy.
Each environment directory is opened by one process at a time.

## Service API

Use `DatalithService` for the task API.
`Media` describes a resource, image, standalone audio item, or HLS video.
Its optional `audio` and `video` fields contain public summaries, while segment details stay in the internal HLS inventory.
`Variant` describes an image output with a name, size multiplier, and format.
`Task` holds the state and result of work that can continue after a restart.

`submit_upload` saves the upload before it returns.
Image and audio/video conversion run in background workers, so the call does not wait for conversion to finish.
FFmpeg is an external process with its own process and encoder-thread limits.

```rust,no_run
use datalith_core::{Datalith, DatalithService, Media, ServiceConfig, TaskStatus, UploadOptions};
use std::time::Duration;

# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let storage = Datalith::new("data").await?;
let service = DatalithService::new(storage, ServiceConfig::default()).await?;
let submitted = service.submit_upload(
    &b"Hello world!"[..],
    UploadOptions { file_name: Some("hello.txt".into()), ..UploadOptions::default() },
    Some("example-upload".into()),
).await?;

loop {
    let task = service.get_task(submitted.id).await?.unwrap();
    if task.status == TaskStatus::Succeeded {
        let media: Media = serde_json::from_value(task.result.unwrap())?;
        println!("{}", media.id);
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

`submit_upload` accepts any reader that implements `AsyncRead + Unpin`, such as a Tokio file or a network stream adapter.
It checks the size limit while reading.
You can pass an idempotency key to avoid creating the same task twice.
The same key with the same input returns the existing task.
The same key with different content or options returns a conflict error.
For `submit_process`, the same key, source ID, and options return the existing task even after the source is deleted or expires.

## Image settings

Enable the `image-convert` feature and pass `ImageOptions` with an image upload:

```rust
use datalith_core::{CropRatio, ImageOptions, ImageVariantSpec, MediaKind, UploadOptions};

let options = UploadOptions {
    kind: MediaKind::Image,
    image: ImageOptions {
        variants: vec![
            ImageVariantSpec {
                name: "avatar".into(),
                max_width: Some(128),
                crop: Some(CropRatio { width: 1.0, height: 1.0 }),
                ..ImageVariantSpec::default()
            },
            ImageVariantSpec {
                name: "preview".into(),
                max_width: Some(640),
                max_height: Some(480),
                ..ImageVariantSpec::default()
            },
        ],
        save_original: true,
        ..ImageOptions::default()
    },
    ..UploadOptions::default()
};
```

Each name must be unique within the request.
Use 1 to 64 ASCII letters, digits, `_`, or `-`; the name `original` is reserved.
Width and height limits must be positive.
EXIF orientation is applied first, followed by an optional center crop and resize.
Resizing keeps the aspect ratio of the cropped image.

Size multipliers default to `[1, 2, 3]`.
They must be unique, include `1`, and fit the configured limit.
The service skips any multiplier that would make the output larger than the source.
Each output reports its actual dimensions, MIME type, content hash, and relative download path.

Every size has a WebP output.
Newly encoded still images also get interlaced PNG when they have an alpha channel, or progressive JPEG otherwise.
GIF, WebP, and APNG animations get animated WebP and interlaced GIF, plus a still PNG or JPEG of the first full frame.
The original file is kept by default.
SVG and SVGZ use `resvg` without external images; the usual WebP and PNG outputs are then created with `image-convert`.
Installed system fonts, embedded raster images, and internal references are supported.
SVG XML and embedded data share a 64 MiB limit, with at most 32 nested SVG images.
The image pixel limits also apply to embedded images.
SVGZ originals keep their compressed bytes and use `application/gzip`.

`submit_process` creates new media from a saved original and leaves the source media available.
It rejects single-use sources.
If the source has an expiry time, the new media keeps that same time.
Processing does not extend its lifetime, and a result that has already expired is not saved.
Media without a retained original cannot be reprocessed.
`ProcessOptions.kind` defaults to `MediaKind::Image` for older clients and also accepts audio or video processing.
It does not accept `MediaKind::Resource`.

Each image worker owns its MagickWand and does not share it with other threads.
Starting a service sets the required ImageMagick policy for the whole process, including static builds.
`ImageLimits` controls pixels per frame, frame count, total decoded pixels, and the number of named settings.
See the [build and deployment guide](../README.md) for native libraries, resource limits, and animation timing limits.

## Audio and video settings

Enable the `av-convert` feature for FFmpeg processing.
It is enabled by default and launches external FFmpeg and ffprobe version 9 or later instead of linking their libraries.
`ServiceConfig.av` selects the programs, the global bitrate, the maximum active FFmpeg processes, and the encoder thread count.
Defaults are `ffmpeg`, `ffprobe`, 12,000,000 bits per second at 1080p/60 fps, one active process, and half the available CPU count with a minimum of one thread.
`capabilities()` separates external tool discovery through `av.available` from AAC, H.264, and FLAC encoder flags.
It reports `av.minimum_tool_major` and an `av.unavailable_reason` for missing, older, or disabled tools.
`media.audio` and `media.video` describe new processing availability, while `video.mp4_export` describes remux availability and can be true without a video encoder.
The service can still handle resources, compatible image processing, and existing media when audio/video processing is unavailable.

```rust
use datalith_core::{AudioOptions, MediaKind, ProcessingMode, UploadOptions};

let options = UploadOptions {
    kind: MediaKind::Audio,
    audio: AudioOptions {
        preserve_lossless: true,
        processing_mode: ProcessingMode::Trust,
        ..AudioOptions::default()
    },
    ..UploadOptions::default()
};
```

Standalone audio creates AAC-LC in M4A at 48 kHz and optionally FLAC with AAC fallback.
AAC first uses 256 kbps and compares its compressed audio payload with the source over the same valid duration.
If that output is larger, it encodes 128 kbps from the source.
The second output may be larger than the source.
Datalith does not produce MP3 fallback.
AAC preserves mono and stereo and mixes other layouts to stereo.
`audio_stream` selects a source stream index; otherwise the source default or first audio stream is used.

`preserve_lossless` adds FLAC for compatible lossless source samples, keeping sample rate, significant bit depth, and channels with compression level 12.
It does not convert a lossy source to FLAC.
A limitation such as floating-point samples that cannot be preserved in FLAC, or a missing encoder when FLAC encoding is needed, returns AAC and a `lossless_not_preserved` warning.
Audio and video do not save the original by default.
Set their `save_original` option to keep a source for later processing.

```rust
use datalith_core::{MediaKind, UploadOptions, VideoOptions, VideoVariantSpec};

let options = UploadOptions {
    kind: MediaKind::Video,
    video: VideoOptions {
        variants: vec![
            VideoVariantSpec { resolution: 720, fps: 30 },
            VideoVariantSpec { resolution: 1080, fps: 60 },
        ],
        preserve_lossless: true,
        ..VideoOptions::default()
    },
    ..UploadOptions::default()
};
```

Video requires explicit resolution and fps pairs and has no default ladder.
Allowed resolution tiers are 144, 240, 360, 432, 480, 540, 576, 720, 900, 1080, 1440, and 2160.
Allowed fps tiers are 10, 12, 15, 20, 24, 25, 30, 48, 50, and 60.
The service adjusts the requested versions downward for the source and combines duplicates.
Sources below 10 fps use 10 fps.
All output canvases are horizontal, including vertical sources.
Content keeps its aspect ratio, fits inside the canvas with black borders, and is not enlarged or cropped.
The output reports its integer fps tier and nominal `Rational` frame cadence.
Endpoint pictures may be held longer to preserve the source audio/video offset, and `leading_hold_seconds` reports an initial hold when needed.

New encoding uses H.264 with x264 `veryslow`, CRF 23, `yuv420p`, progressive frames, and SDR color.
The service normalizes rotation and pixel aspect ratio and converts interlaced or HDR input when needed.
The H.264 level, references, rate limit, and buffer size depend on the effective output.
The [HTTP guide](../datalith/README.md#video-and-hls) lists exact canvases and the global bitrate scaling formula.

Video normally stores HLS VOD with separate fMP4 video and shared audio tracks.
Below 720p, only the lower AAC track is allowed; from 720p, the higher AAC track is allowed when available; from 1080p, FLAC is also allowed when available.
`VideoVariant.audio` lists these allowed track identifiers.
A source with no audio creates video-only playlists.
`VideoMedia.master_path` names the default AAC master playlist, and each variant reports its playlist path.
Clients can request all audio or FLAC combinations after checking playback support.

## HLS and playback sessions

`claim_playback_session(id, key)` claims single-use audio or video and returns `PlaybackSession`.
Read the Media summary before claiming, then pass `Some(session.token.as_str())` to protected reads.
Ordinary media use `None` and need no session claim.

| Method | Use |
| --- | --- |
| `hls_master(id, filter, token)` | Render combinations using `HlsAudioFilter::Aac`, `All`, or `Flac` |
| `hls_track(id, track, token)` | Render one video or audio track playlist |
| `open_hls_content(id, track, asset, token)` | Open `HlsAsset::Initialization` or `HlsAsset::Segment(sequence)` |

Both playlist methods return `HlsPlaylist` with its `body` and a `temporary` flag for cache policy.
Segment sequence numbers start at zero.
Public Media paths are relative to the service root, while M3U8 child URLs are relative to their containing playlist.
Master child paths use `{track}/index.m3u8`, and track assets use `init.mp4` or `segment-000000.m4s`.
Authorized single-use playlists append the same session token to child URLs.
Ordinary playlists do not echo a supplied token.
HLS and playback-session methods remain available when audio/video processing is disabled.

## Trust mode

`ProcessingMode::Transcode` is the default for image, audio, and video options.
`ProcessingMode::Trust` checks each output separately and can copy a compliant file or remux a compliant compressed stream.
It does not require the original encoder or quality settings to match the service recipe.
`ProcessingMethod` reports `Copied`, `Remuxed`, or `Transcoded`; older metadata can use `Unknown`.
Warnings contain a stable code and a message for people to read.

Images always provide WebP.
A compliant JPEG or PNG can be reused as its matching fallback, while a matching WebP can be reused as the main output.
The same no-upscale and multiplier rules still apply.
A crop, orientation, color, size, or animation change can require conversion.

Audio checks include the codec, sample rate, channel layout, and bitrate.
Video also checks H.264 profile and level, canvas size, fps, pixel format, timestamps, packet bitrate, and independent segment boundaries.
A matching average bitrate or MP4 `faststart` flag alone is not enough.
The service may encode when a necessary requirement cannot be confirmed.
Video and audio can be reused independently before packaging as HLS.
When an output references the uploaded bytes, those bytes remain stored even if `save_original` is false.

## Read and delete media

`get_media` and `list_media` return metadata.
`open_content` returns a `Content` value with a Tokio file and a read guard.
The guard prevents the stored file from being deleted while the `Content` value is alive.
`Content.repeatable` is true for ordinary reads and authorized playback-session reads, including byte ranges.
`Content.single_use` still identifies the retention rule, and `temporary` still requires a no-store cache policy.
Use `ContentRequest` to select an image name, multiplier, and format, or an audio output and format.
For standalone audio, follow each `AudioVariant.content_path` or select `m4a` or `flac` when that output exists.
Use `variant: Some("original".into())` to read the original file.
For images, the default is the first name's 1x WebP output.

`Retention` keeps media forever by default.
Set `expires_in_seconds` to allow repeated downloads until expiry.
For a new upload, this period starts when processing is complete and the media is saved.
For resources and images, set `single_use` to allow only one download claim.
For audio and video, it instead allows one playback-session claim for repeated reads during a fixed session.
You can use both settings together.

For resources and images, reading metadata or calling `open_content(..., true)` for HEAD does not use that claim.
For resources and images, a normal content open claims the single-use download before it returns the file.
Only one caller can claim it, even when requests arrive at the same time.
An interrupted resource or image download does not restore the claim.

Audio and video sessions allow segments, version changes, seeking, and replay while the token is valid.
The default session lasts 24 hours, is not extended by reads, and cannot outlive the media.
The first claim and its idempotent retries use the same expiry with millisecond precision.
Tokens have 64 lowercase hexadecimal characters.
Claiming it hides the media from ordinary metadata reads and lists, so read the summary first.
Tokens are not part of the public Media or Task result.
Deleting the media invalidates its session.
The HTTP interface passes the token as the `session` query parameter.
Use `open_content_with_session(id, request, head, token)` for single-use audio or video originals and content.
The older `open_content` wrapper supplies no token and still serves ordinary media and the resource/image download rules.

`delete_media` removes the media and its file references.
A file stays on disk while other media or active readers still use it.
Cleanup removes the file after all references and readers are gone.
`close` stops and waits for workers before closing the database.

## Tasks and archives

Task states are `queued`, `running`, `cancelling`, `succeeded`, `failed`, and `cancelled`.
`cancel_task` asks image workers to stop between processing steps and stops active FFmpeg work.
`retry_task` retries a failed or cancelled task while its input is still available.
Successful tasks remove their temporary input and work files right away.
Failed and cancelled tasks keep their input for retry, for seven days by default.
Finished task records and TAR transfer archives use the same retention period.
MP4 artifacts use `mp4_export_retention_seconds`, which defaults to 24 hours, and task records remain until their artifacts expire.
New processing tasks snapshot the effective output recipe for stable retries.
On restart, the service reads SQLite and queues unfinished work again.

`submit_export(ExportOptions { ids: None }, ...)` exports all available media.
Pass a list of IDs to export only those items.
Use `open_artifact` to read the finished archive.
`submit_import` reads and checks an archive, then merges its media into the current environment.
If an ID is already used by different data, the import gives it a new ID and returns the mapping.
The archive stores media and file contents, not a copy of the SQLite database.

`submit_mp4_export(id, options, token, key)` selects one existing `VideoVariant.id` and copies its compressed video with the best allowed existing audio: FLAC, higher AAC, then lower AAC.
It uses `faststart` and does not encode again.
`Mp4ExportResult` contains the source `media_id`, `variant`, selected `audio` identifier or `None`, artifact metadata and path, and `expires_at`.
The result is a temporary artifact, while normal video storage remains HLS.
Its default 24-hour lifetime is capped by source expiry and any required playback session.
Creating it needs `av-convert` and usable tools, while reading existing media and completed artifacts does not.
`open_artifact_with_session(id, token)` reads either a TAR or MP4 artifact with the credential when required.
`open_artifact(id)` remains the wrapper for requests without a credential.
For an MP4 export of single-use video, use `cancel_task_with_session(id, token)` and `retry_task_with_session(id, token)` with the same active session.
The older cancellation and retry wrappers remain available for other tasks.
Check `capabilities()` before a client selects upload or reprocessing types, formats, or limits.

Archive version 2 includes all media summaries, saved originals, image and audio outputs, and the complete HLS inventory and contents.
Each content hash is stored only once.
Version 1 archives remain readable.
Sessions, task history, configuration, and temporary MP4 artifacts are not included.

## Older storage API

`DatalithFile`, `DatalithResource`, `DatalithImage`, and the direct storage methods are still available.
New applications should use `DatalithService` throughout, without mixing it with direct writes through the older API.
Database versions 1 and 2 are upgraded to version 3 with a versioned database backup.
The first migration turns existing resources and images into media with the same IDs.
Files that are not part of a resource or image also keep their IDs.
Migration does not rebuild thumbnails or guess old crop settings.

## Build features

Rust 1.94 or later is required, and CI checks Rust 1.94.1.
The default features are `magic`, `image-convert`, `av-convert`, and `manager`.
`manager` provides the older cleanup scheduler; the new service task queue does not need it.
Disable default features to build without native image or MIME libraries and FFmpeg processing.
See [FFMPEG.md](../FFMPEG.md) for the fixed external tool build and its GPL licenses and corresponding sources.
The Datalith library remains MIT licensed.

## Links

[Crates.io](https://crates.io/crates/datalith-core) | [API documentation](https://docs.rs/datalith-core)

## License

[MIT](LICENSE)
