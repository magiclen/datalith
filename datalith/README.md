# Datalith service guide

Datalith stores files and prepares images, audio, and video through background tasks.
The service and Rust library support Linux.
Start with the [project overview](../README.md) and use this guide when you need to change settings or connect an application.

## Docker deployment

From the project folder, start the full service:

```sh
docker compose up --build -d
```

For ordinary file storage without media conversion:

```sh
docker compose -f docker-compose.yml -f docker-compose.files.yml up --build -d
```

Use the same two `-f` arguments for later commands on the file-only deployment.
Both versions use the same data folder and local port.
Only run one version against that folder at a time.
The file-only version can still serve existing processed media, but it cannot create new conversions or MP4 exports.

The data folder is `~/docker/datalith/db`, mounted at `/app/data` in the container.
For a new Linux deployment, create it with `sudo install -d -o 1000 -g 1000 "$HOME/docker/datalith/db"` before starting.
Existing files must remain writable by UID 1000.
To choose another folder, change only the host path in `volumes`.

The port mapping is `127.0.0.1:1111:1111`.
To use local port 2222, change it to `127.0.0.1:2222:1111`; the port inside the container stays 1111.
Then open `http://127.0.0.1:2222/api/v1/docs`.

Check the service with `docker compose ps` and `docker compose logs --tail=100 app`.
Logs are kept in up to three 10 MB files per container.
Use `docker compose stop` to stop and `docker compose start` to start again.
Stopping or replacing the container does not remove the data folder.

## Service settings

Service settings control storage limits and machine resources.
In Docker, put them under `environment` in `docker-compose.yml`, then run `docker compose up -d` to apply changes.
Leave a setting out to use its default.

| Setting | Default | Meaning and when to change it |
| --- | --- | --- |
| `DATALITH_MAX_FILE_SIZE` | `2 GiB` | The largest single upload or import archive. Increase it for larger files; it does not limit total stored data. |
| `DATALITH_WORKERS` | `1` | Tasks that can run at once. Increase it for more parallel work if the machine has enough CPU and memory. |
| `DATALITH_FFMPEG_PROCESSES` | `1` | FFmpeg or ffprobe processes that can run at once. Increase it with the worker count when more audio/video work should run in parallel. |
| `DATALITH_FFMPEG_THREADS` | Half the available CPUs, at least 1 | Encoder threads per process. Lower it to leave CPU capacity for other work. More threads do not always mean faster encoding. |
| `DATALITH_BITRATE` | `12000k` | The video rate limit at 1080p/60 fps. Lower it to reduce peak bandwidth, or raise it to allow more detail in difficult scenes. |
| `DATALITH_TASK_RETENTION_SECONDS` | `604800` (7 days) | How long finished task records, transfer archives, and failed inputs kept for retry remain available. |
| `DATALITH_PLAYBACK_SESSION_SECONDS` | `86400` (24 hours) | How long a single-use audio/video playback claim lasts. It cannot outlive the media. |
| `DATALITH_MP4_EXPORT_RETENTION_SECONDS` | `86400` (24 hours) | How long completed MP4 downloads remain available. Source and session expiry can shorten this. |
| `DATALITH_MAX_IMAGE_RESOLUTION` | `50000000` | Maximum pixels in one source frame, not an output width or height. Lower it to reject very large images. |
| `DATALITH_MAX_IMAGE_FRAMES` | `500` | Maximum frames in an animation. Lower it to limit long animations. |
| `DATALITH_MAX_IMAGE_TOTAL_PIXELS` | `100000000` | Maximum decoded pixels across all frames. Lower it to reduce animation memory use. |
| `DATALITH_MAX_IMAGE_VARIANTS` | `16` | Maximum named image size settings per upload. |
| `DATALITH_MAX_IMAGE_RESOLUTION_MULTIPLIER` | `3` | Largest allowed image scale, such as 3x. The service still does not enlarge the source. |

Image and FFmpeg settings apply to builds with their conversion features enabled.
The video bitrate is a limit, not a fixed output bitrate.
Other video levels use a rate based on their size and fps, and each buffer is two seconds of that rate.
For `12000k` at 1080p/60 fps, FFmpeg uses `maxrate=12000k` and `bufsize=24000k`.

For example, this limits uploads to 1 GiB and lowers the video rate limit:

```yaml
environment:
  DATALITH_MAX_FILE_SIZE: 1 GiB
  DATALITH_WORKERS: 1
  DATALITH_BITRATE: 8000k
```

These are service settings.
Conversion switches, output sizes, `save_original`, `preserve_lossless`, and `retention` are settings for each upload.
The task retention setting does not expire your stored media.

Without Docker, use the same environment variables or command-line options:

```sh
datalith --environment ./data --max-file-size '1 GiB' --bitrate 8000k
```

`DATALITH_ENVIRONMENT` selects the data folder (default `.`).
`DATALITH_LISTEN_PORT` selects the listening port (default `1111`).
`DATALITH_ADDRESS` selects the listening address (default `0.0.0.0` in release builds, `127.0.0.1` in debug builds).
Command-line options take priority over their environment variables.
Use `datalith --help` for all options, including custom FFmpeg and ffprobe paths.

## Upload files

Every type uses `POST /api/v1/uploads` with one multipart `file` and an optional JSON `options` field.
Without options, the file is saved as a permanent resource without conversion.

```sh
curl -F 'file=@./hello.txt' http://127.0.0.1:1111/api/v1/uploads
```

Enable the conversions you want to allow and send their settings:

```sh
curl -F 'file=@./photo.jpg' \
  -F 'options={"enable_convert_to_image":true,"enable_convert_to_audio":true,"enable_convert_to_video":true,"image":{"variants":[{"name":"preview","max_width":1080,"max_height":1080,"multipliers":[1,2,3]}]},"video":{"variants":[{"resolution":720,"fps":30},{"resolution":1080,"fps":60}]}};type=application/json' \
  http://127.0.0.1:1111/api/v1/uploads
```

Datalith checks the contents and selects a matching enabled type.
For example, the same options can process a photo, song, or video, while a PDF stays a resource.
File names and the supplied MIME type do not choose the conversion type.
Each enabled type must have valid settings and available processing support.
A matching file that cannot be processed makes the task fail; it is not saved as a resource instead.

All conversion switches default to `false`.
Supplying an `image`, `audio`, or `video` object does not turn its switch on.
The existing manual `kind: "image"`, `"audio"`, or `"video"` requests remain supported, but cannot use automatic switches at the same time.
Automatic requests can omit `kind` or set it to `"resource"`.

### Follow the task

The response is `202 Accepted` with a task ID after the file transfer and task save finish.
It does not wait for conversion.
Poll the returned ID:

```sh
curl http://127.0.0.1:1111/api/v1/tasks/TASK_ID
```

Wait for `succeeded`, then read the Media object in `result`.
Automatic uploads have task kind `upload`; `result.kind` reports `resource`, `image`, `audio`, or `video`.
`failed` includes an error code and a readable message.
You can request cancellation with `POST /api/v1/tasks/TASK_ID/cancel` or retry failed and cancelled tasks with `POST /api/v1/tasks/TASK_ID/retry`.

An `Idempotency-Key` header lets a repeated request return the same task while its record is kept.
Reusing a key with different content or settings returns a conflict.

## Media settings

### Images

Use named `image.variants` to set size limits, optional center crops, and 1x/2x/3x outputs.
The source orientation is applied first, and smaller sources are not enlarged.
Without size limits, the default keeps the source size and creates only 1x outputs.

Every size has WebP, with PNG fallback for transparency or JPEG otherwise.
GIF, WebP, and APNG animations also have GIF and a still first-frame fallback.
SVG and SVGZ are supported, but external image paths and URLs are rejected.
New JPEG outputs are progressive; PNG and GIF outputs are interlaced.

### Audio

AAC-LC uses 48 kHz, with mono or stereo; other channel layouts are mixed to stereo.
Lossless sources use a 256 kbps target without comparing compressed sizes.
For lossy sources, the service tries 256 kbps first and compares its compressed audio payload with the source over the same duration.
If that output is larger, it encodes 128 kbps from the source instead; that fallback may still be larger than the source.

Set `audio.preserve_lossless` to keep compatible lossless samples as FLAC, with AAC fallback.
FLAC keeps the source sample rate, bit depth, and channels and uses maximum compression.
When preservation is not possible, the task returns AAC with a warning.
Use `audio.audio_stream` to select a source audio stream when the default is not the one you need.

### Video

Video conversion needs explicit `video.variants` with resolution and fps pairs; there is no default ladder.
Available resolution tiers are 144, 240, 360, 432, 480, 540, 576, 720, 900, 1080, 1440, and 2160.
Available fps tiers are 10, 12, 15, 20, 24, 25, 30, 48, 50, and 60.
Requests are adjusted downward for the source, except sources below 10 fps use 10 fps.
Duplicate effective versions are combined.

Outputs use H.264, x264 `veryslow`, CRF 23, `yuv420p`, and horizontal canvases with black borders where needed.
The content keeps its shape and is not enlarged or cropped.
HDR and interlaced sources are converted to SDR and progressive video.

HLS stores video and shared audio tracks separately.
Versions below 720p use lower-bitrate AAC; 720p and above can also use higher-bitrate AAC, and 1080p and above can use FLAC when available.
Set `video.preserve_lossless` to request lossless source audio, with AAC fallback.
The default HLS master uses AAC; a compatible client can select FLAC and return to AAC when needed.
HLS provides the quality choices; the player decides when to switch.

### Originals and trust mode

Images keep the original by default; audio and video do not.
Set the matching `save_original` to keep a source for later processing.
`POST /api/v1/media/MEDIA_ID/tasks` creates new media from a saved original and leaves the old item unchanged.
This request requires `kind` to select `image`, `audio`, or `video`:

```sh
curl -H 'Content-Type: application/json' \
  -d '{"kind":"image","image":{"variants":[{"name":"preview","max_width":640,"max_height":640}]}}' \
  http://127.0.0.1:1111/api/v1/media/MEDIA_ID/tasks
```

Leaving out `kind` returns `422` with error code `invalid_request`.

Set `processing_mode: "trust"` inside the image, audio, or video options to reuse compliant files or streams.
Datalith still checks each output and converts anything that does not meet its requirements.
A matching MP4 is packaged into HLS, and an image still needs WebP.

## Get files and play media

Use `GET /api/v1/media/MEDIA_ID` to read metadata and `GET /api/v1/media` to list items.
Use `/api/v1/media/MEDIA_ID/content` for a resource, the default image, or standalone audio.
For other outputs, use the paths returned in the Media object.
Resolve those paths against the service root, including any reverse proxy prefix.

For video, use `video.master_path` with an HLS player or open `/api/v1/player` to try the [playback example](../examples/player/README.md).
Use `POST /api/v1/media/MEDIA_ID/mp4-exports` with `{"variant":"1080p60"}` to export an existing video version.
It copies the stored streams into MP4 without encoding again and selects the best allowed audio: FLAC, higher AAC, then lower AAC.
A FLAC MP4 needs a compatible player.
After the task succeeds, use its returned artifact path to download it before expiry.

## Temporary media

Set `retention.expires_in_seconds` to remove media after a chosen time, starting when processing finishes.
Leave it out to keep media permanently.
This is separate from task history and export retention.

Set `retention.single_use` for one access claim.
For resources and images, the first content GET claims the download; an interrupted download does not restore it.
For audio and video, read the metadata first, then claim a playback session with `POST /api/v1/media/MEDIA_ID/playback-sessions`.
Pass its `token` as `session=TOKEN` to protected content, HLS, and MP4 requests.
The session allows seeking and replay until expiry; it is one claim, not one viewing.
Use an `Idempotency-Key` for the claim so a retry can recover the same token.

## Move data, back up, and upgrade

Use `POST /api/v1/exports` with `{}` to export all available media, or `{"ids":["MEDIA_ID"]}` for selected items.
After the task succeeds, download its artifact.
Upload that TAR file to `POST /api/v1/imports` using the multipart `file` field on another service.
The import result reports any ID changes needed to avoid conflicts.

Archives contain media, saved originals, and generated outputs, including HLS segments, with one copy of each unique file.
They do not transfer server settings, task history, playback sessions, or temporary MP4 exports.
Export briefly pauses writes and new single-use claims, while ordinary downloads continue.

The standalone CLI can also move data:

```sh
datalith --environment ./source export ./media.tar
datalith --environment ./destination import ./media.tar
```

Stop any service using those data folders before running the CLI.
Use HTTP transfers when the service must stay running.

Before upgrading, stop the service and copy the whole data folder to a safe place.
Do not back up only `datalith.sqlite`; the folder also holds file contents and pending work.
Rebuild and start with `docker compose up --build -d` after the backup.
Startup upgrades the original version 1 database to version 2 and keeps media IDs and stored outputs.
It saves `datalith.sqlite.v1.bak` before the upgrade and removes the old resource and image tables after their data has been moved.
This database backup is not a full data backup.
Development database formats between these versions are not supported.
The old HTTP API and Node.js client are not compatible with `/api/v1`.

Old Docker deployments used `Dockerfile.image` and `docker-compose.image.yml`.
The full build now uses `Dockerfile` with the `media` target, and the start command is `docker compose up --build -d`.
The host data location is unchanged; `/app/shared/db` inside the old container is now `/app/data`.

## API and native builds

Open `/api/v1/docs` for Swagger UI and `/api/v1/docs/json` for the generated OpenAPI document.
The UI is included in the service and needs no CDN.
`/api/v1/capabilities` reports available processing features and limits.
Use error codes rather than message text when an application handles errors.

Native builds need Linux, Rust 1.94 or later, libmagic development files, and ImageMagick 7.1.1 or later below 7.2.
Image builds also need pkg-config and Clang.
Audio/video processing needs external FFmpeg and ffprobe version 9 or later.
Docker uses ImageMagick 7.1.2-32 with FFmpeg 9.0.2; older ImageMagick animation delegates may need an upgrade.
Docker includes the tested tools; see [FFMPEG.md](../FFMPEG.md) for their build and source bundle.

```sh
cargo build --locked --release -p datalith
./target/release/datalith --environment ./data
```

For development, use `cargo +nightly fmt --all`, the workspace tests, and clippy with all targets and features.
To run the tests with the same FFmpeg and ImageMagick builds as CI, use `make docker-test`.
It runs `docker compose -f docker-compose.test.yml run --build --rm test`; add a command after `test` to run something else, such as `cargo test --locked --workspace --no-default-features`.
The source folder is mounted into the container and must be writable by UID 1000; the first build takes time because it builds the media tools.
See the [CI workflow](../.github/workflows/ci.yml) for the checked build combinations.

## License

[MIT](LICENSE)
