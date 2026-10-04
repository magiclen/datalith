# Datalith

Datalith is a small media storage service that runs as one instance.
It keeps file contents on the local file system and stores metadata and tasks in SQLite.
Files with the same SHA-256 hash share one stored copy.
It does not need a separate database or queue server.

## Features

- Upload a file, get a task ID, and poll `/api/v1` for the result.
- Keep files permanently, set an expiry time, or allow only one download.
- Crop and resize one image into several named sizes, with 1x, 2x, and 3x versions when the source is large enough.
- Serve WebP with JPEG or PNG fallbacks, with progressive or interlaced encoding for new outputs.
- Read GIF, WebP, and APNG animations and create animated WebP, interlaced GIF, and a still fallback.
- Convert standalone audio to AAC in M4A, with optional lossless FLAC and an AAC fallback.
- Create H.264 video versions in HLS with shared audio tracks and adaptive playback.
- Reuse compliant image files and audio or video streams in trust mode.
- Claim one playback session for single-use audio or video, with seeking and replay during its lifetime.
- Remux an existing video version into a temporary MP4 export.
- Export all media or selected IDs to a TAR archive and import it into another Datalith service.

Images always have a WebP main version and never upscale their content.
Audio uses native AAC-LC at 48 kHz, and Datalith does not produce an MP3 fallback.
Video uses horizontal canvases, preserves the content aspect ratio, and adds black borders where needed.
Video versions must be requested explicitly as resolution and fps pairs.
The service normally stores video as HLS fMP4 streams and creates complete MP4 files only for export tasks.
Images keep the original upload by default, while audio and video do not.
Set `save_original` when later reprocessing is needed.

See the [HTTP and CLI guide](datalith/README.md) and the [Rust API guide](datalith-core/README.md).
A running service provides Swagger UI at `/api/v1/docs`, its generated OpenAPI JSON at `/api/v1/docs/json`, and feature information at `/api/v1/capabilities`.
The minimal [browser player example](examples/player/README.md) is served at `/api/v1/player`.
See [FFmpeg build and distribution](FFMPEG.md) for fixed source versions, checksums, licenses, and the source bundle.
The old [Node.js client](https://github.com/magiclen/node-datalith) does not support this API.
A new client library will be built in a separate project.

## Processing and playback

Processing defaults to `transcode`.
Set `processing_mode` to `trust` for an image, audio, or video upload to check the source and reuse compliant outputs.
Each output reports whether it was `copied`, `remuxed`, or `transcoded`.
The service still creates WebP for a JPEG or PNG image, and can reuse the source as a matching fallback.
It reuses video and audio streams separately, then packages video outputs as HLS.
The codec, canvas, fps, color format, timestamps, packet bitrate, and segment boundaries must meet the output requirements.
A matching average bitrate or `faststart` flag alone is not enough.
Trust mode does not require the original encoder or its quality settings to match the service recipe.

AAC uses a 256 kbps first choice and a 128 kbps second choice.
If the first output has a larger audio payload than the source, the service encodes the second choice from the source.
The second choice may be larger than the source.
Single-channel and stereo sources keep their channel count, and other layouts are mixed to stereo for AAC.
`preserve_lossless` keeps a compatible lossless source in FLAC with its sample rate, bit depth, and channel count, plus AAC fallback.
When lossless preservation is not possible, processing succeeds with AAC and a warning.
Only one audio stream is selected, using an explicit source stream index, the source default, or the first audio stream.

HLS stores audio and video separately and shares each audio track across allowed video versions.
Video below 720p allows the lower AAC choice, 720p and above also allow the higher AAC choice when available, and 1080p and above also allow FLAC when available.
The default master playlist contains AAC combinations.
Clients can request FLAC combinations after checking decoder support and fall back to AAC when playback needs it.
The browser example demonstrates bandwidth and buffer based playback choices; formal client libraries are separate projects.

MP4 export selects an existing video version and the best audio allowed with it, in the order FLAC, higher AAC, then lower AAC.
It copies the existing compressed streams, adds `faststart`, and does not encode them again.
Completed MP4 files last 24 hours by default, independently of task history retention, and cannot outlive the source expiry or a required playback session.
Creating a new MP4 requires the audio/video processing feature and tools, while existing HLS, audio, sessions, and completed artifacts remain readable without them.

## Build and run

The project uses Rust edition 2024 and requires Rust 1.94 or later.
CI checks the minimum supported Rust version (MSRV) with Rust 1.94.1.
MIME detection needs the libmagic development package.
Image support also needs ImageMagick development headers, pkg-config, and Clang.
Audio and video processing require external FFmpeg and ffprobe version 9 or later, with version 9.0.2 used by Docker and CI.
The default `av-convert` feature enables their integration.
Capabilities separate external tool discovery, audio/video encoders, and MP4 remux availability; copying existing video into MP4 does not require a video encoder.
If the tools are missing or too old, capabilities report that audio/video processing is unavailable and explain the reason.
The service can still store resources, use image processing with its compatible ImageMagick and FFmpeg combination, and serve existing media.
For a musl build with MIME detection, use a musl build of libmagic and set `MAGIC_DIR` and `MAGIC_STATIC=1`.
The ImageMagick build variables do not configure libmagic.
Use `--no-default-features --features image-convert` to keep image processing without native MIME detection.
The ImageMagick version range supported by `magick_rust` is `>= 7.1.1, < 7.2`.
Docker and CI pair FFmpeg 9.0.2 with ImageMagick 7.1.2-32.
Older ImageMagick deployments must be upgraded if their animation delegate still uses options removed by FFmpeg 9.

```sh
cargo build --locked --release -p datalith
./target/release/datalith --environment ./data
```

For a smaller service without image, audio, or video conversion:

```sh
cargo build --locked --release -p datalith --no-default-features --features magic
```

Image conversion uses `image-convert` 0.23.
ImageMagick must support PNG, JPEG, WebP, and GIF.
Animated WebP needs libwebpmux, and APNG needs an ImageMagick coder and an FFmpeg delegate.
A delegate is an external program that ImageMagick calls to read or write a format.
Keep both the ImageMagick shared libraries and its configuration directory when you deploy the service.
The service applies the built-in policy from `datalith-core/src/service/image_policy.xml` before it starts its workers.
This also works with static, zero-configuration ImageMagick builds that ignore external XML files.
Startup fails if ImageMagick cannot apply the required policy.
The Docker image and CI install the same policy for command-line tools.
It blocks formats that need Ghostscript, such as PostScript, EPS, PDF, PCL, and XPS.
SVG and SVGZ files are rendered with `resvg`, then passed to `image-convert` for the usual output formats.
External image paths and URLs are rejected, including those in embedded SVG images.
SVG text uses installed system fonts; embedded raster images and internal references are supported.
SVG XML and embedded data share a 64 MiB limit, with at most 32 nested SVG images; image pixel limits also apply.
SVGZ originals keep their compressed bytes and use the `application/gzip` MIME type.
APNG decoding and the image cache need a writable temporary directory.

The default image limits are 50 million pixels per frame, 500 frames, 100 million decoded pixels in total, and 16 named image settings.
The service also limits ImageMagick to 256 MiB of memory cache, 512 MiB of mapped cache, 2 GiB of disk cache, and one thread.
These limits apply to the whole process.
A lower limit from an existing ImageMagick policy stays in place.

APNG frame delays are rounded to 10 ms.
GIF frames are combined into full frames before editing and are not optimized again, so the output may be larger than the source.
WebP does not have the same progressive display mode as JPEG.
Task cancellation takes effect between processing steps.
A native image encoder or delegate that is already running must finish its current step first.
FFmpeg work supports cancellation and process shutdown, with a configurable limit on active processes and encoder threads.

## Docker

```sh
docker compose -f docker-compose.image.yml up --build -d
```

Use `docker-compose.yml` for the lightweight file service.
`docker-compose.image.yml` provides image, audio, and video conversion.
Both Dockerfiles use Debian Bookworm for the build and runtime stages, with Rust 1.99.0 in the builder.
The image build uses [ImageMagick 7.1.2-32](https://github.com/ImageMagick/ImageMagick/releases/tag/7.1.2-32) and checks the SHA-256 hash of its source archive.
The full runtime includes FFmpeg 9.0.2, libwebpmux, and the ImageMagick delegate settings.
Its FFmpeg build enables GPL libx264 and includes the exact corresponding sources and build script.
The `NATIVE_BUILD_JOBS` Docker build argument defaults to two for both native builds.

Compose stores data in `~/docker/datalith` and binds port 1111 to localhost.
The mounted directory must allow UID 1000 to write files.
Datalith has no built-in login or access control.
Use a trusted backend or a reverse proxy with authentication for public access.
The proxy must stream multipart uploads and allow enough time for each upload to finish.
Media processing continues after the `202 Accepted` response.

Keep the whole environment directory on persistent storage.
It contains `datalith.sqlite`, `datalith.files`, the old API's `datalith.temp`, and the task directory `datalith.tasks`.
Only one process can open an environment at a time.
The default worker count is one; more workers still run inside that same process.

## Upgrade existing data

Version 0.2 replaces the old HTTP API.
At startup, Datalith upgrades database versions 1 and 2 to version 3 and keeps the existing File, Resource, and Image IDs.
Before changing an existing older database, it creates `datalith.sqlite.v1.bak` or `datalith.sqlite.v2.bak` for that source version.
The backup is written to a temporary file and renamed only after it is complete.
Metadata changes are then saved in one database transaction.

Existing image outputs become variants under the `default` name and are not converted again.
Old crop settings were not stored, so their recipes stay unknown.
Images without an original file can still be downloaded, but they cannot be processed into new sizes.

Stop the old process and back up the whole environment directory before upgrading.
The automatic versioned backup contains only the database.
The new service may remove duplicate file contents, so that database file alone is not enough to restore the old environment.
If migration fails, the HTTP service does not start; restart the process to try again.
A database from a newer, unsupported version is rejected.

Tasks and their input files survive a restart.
Interrupted work is queued again.
Saving the media result and marking its task as successful happen in the same database transaction.
Successful tasks remove their temporary input and work files right away.
Failed and cancelled tasks keep their input for retry, for seven days by default.
Finished task records and transfer archives use the same retention period.
MP4 artifacts have their own retention period, and their task records stay at least until the artifact expires.
Each new processing task saves its effective output settings so that retries keep the same recipe.
Unused files are removed only after their database references and active readers are gone.
Regular cleanup checks released storage IDs and retries content that is still open.
A full scan runs at startup and once per hour to find files left by interrupted work.

## Import and export

The CLI needs exclusive access to the environment, so stop its HTTP service before running these commands:

```sh
datalith --environment ./source export ./media.tar
datalith --environment ./destination import ./media.tar
```

Use the HTTP import and export endpoints while the service is running.
Archive version 2 contains a JSON manifest and one copy of each unique content file, including saved originals, image outputs, standalone audio, and all HLS initialization files and segments.
Import also accepts version 1 archives.
Playback sessions, task history, server settings, and temporary MP4 exports are not transferred.
It leaves out expired media, used single-use media, and unfinished uploads.
Export pauses content writes and cleanup, while normal downloads continue.
New single-use content or playback-session claims are paused because they need to record the claim.
Reads through an already claimed playback session continue.

Import checks the archive before it merges any media.
It skips matching items and gives new IDs to items whose IDs are already used by different data.
The task result contains the old-to-new ID maps.
Importing the same archive again returns its saved mapping.
Archives move media between services; they do not include task history or server settings.

## Development

```sh
cargo +nightly fmt --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo test --locked --workspace --no-default-features
```

CI uses the shared `.github/scripts/install-ffmpeg.sh` and the ImageMagick installer for fixed media tools on Ubuntu.
Install the same dependencies before running default-feature tests, as described in [FFMPEG.md](FFMPEG.md).
The MSRV job uses `--lib --bins` on purpose.

## License

[MIT](LICENSE)

The FFmpeg programs in the full Docker image use GPL version 2 or later.
Their licenses and corresponding source bundle are documented in [FFMPEG.md](FFMPEG.md).
