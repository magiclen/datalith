# Datalith

Datalith stores file contents on disk and keeps metadata in SQLite.
Files with the same SHA-256 hash share one stored copy.
One image upload can produce several crops, sizes, multipliers, and formats.
Audio outputs use AAC in M4A and optional lossless FLAC.
Video outputs use H.264 in HLS with separate shared audio tracks.
Conversion, import, and export run as tasks that can recover after a process restart.

The API uses the `/api/v1` prefix and is not compatible with the old API.
At startup, migration upgrades old data and keeps its IDs.
Clients must use the new routes and data model.
The old `node-datalith` package does not support this API; a new library will be built in a separate project.

## Start and configure the service

```sh
cargo run --release -p datalith -- --environment ./data
# You can also use the serve command.
cargo run --release -p datalith -- --environment ./data serve
```

Only one Datalith process can open an environment directory at a time.
This applies to the HTTP service, the CLI, and the Rust library.
Stop the HTTP service before using the CLI to import or export from the same directory.
Use the HTTP API to move data while the service is running.

| Option | Environment variable | Default |
| --- | --- | --- |
| `--environment` | `DATALITH_ENVIRONMENT` | `.` |
| `--address` | `DATALITH_ADDRESS` | `127.0.0.1` in debug builds; `0.0.0.0` in release builds |
| `--listen-port` | `DATALITH_LISTEN_PORT` | `1111` |
| `--max-file-size` | `DATALITH_MAX_FILE_SIZE` | `2 GiB`, also used for import archives |
| `--workers` | `DATALITH_WORKERS` | `1`; allowed range is 1 to 64 |
| `--task-retention-seconds` | `DATALITH_TASK_RETENTION_SECONDS` | `604800` (seven days) |
| `--max-image-resolution` | `DATALITH_MAX_IMAGE_RESOLUTION` | `50000000` pixels per frame |
| `--max-image-resolution-multiplier` | `DATALITH_MAX_IMAGE_RESOLUTION_MULTIPLIER` | `3` |
| `--max-image-frames` | `DATALITH_MAX_IMAGE_FRAMES` | `500` |
| `--max-image-total-pixels` | `DATALITH_MAX_IMAGE_TOTAL_PIXELS` | `100000000` |
| `--max-image-variants` | `DATALITH_MAX_IMAGE_VARIANTS` | `16` |
| `--bitrate` | `DATALITH_BITRATE` | `12000k` (12,000,000 bits per second at 1080p/60 fps) |
| `--ffmpeg` | `DATALITH_FFMPEG` | `ffmpeg` |
| `--ffprobe` | `DATALITH_FFPROBE` | `ffprobe` |
| `--ffmpeg-processes` | `DATALITH_FFMPEG_PROCESSES` | `1` |
| `--ffmpeg-threads` | `DATALITH_FFMPEG_THREADS` | Half the available CPU count, at least one |
| `--playback-session-seconds` | `DATALITH_PLAYBACK_SESSION_SECONDS` | `86400` (24 hours) |
| `--mp4-export-retention-seconds` | `DATALITH_MP4_EXPORT_RETENTION_SECONDS` | `86400` (24 hours) |

Image options are available only with the `image-convert` feature.
FFmpeg processing options are available only with the `av-convert` feature.
New audio/video processing requires both FFmpeg and ffprobe version 9 or later.
Missing or older tools disable that processing in capabilities with a reason, while resources, compatible image processing, and existing media remain available.
`bitrate` is a global video limit and cannot be changed by an upload.
The FFmpeg process limit is separate from the task worker count.
`--temporary-file-lifespan` and `DATALITH_TEMPORARY_FILE_LIFESPAN` apply to the older Rust API.
For the new HTTP API, set each upload's expiry with `retention`.

The service has no built-in login or access control.
Use a trusted backend or reverse proxy to handle authentication, TLS, and public traffic limits.
The proxy must support streaming multipart uploads and allow enough time and space for them.
The service returns `202` after it saves the upload and task, so the file transfer still needs to finish first.

## Upload files and check tasks

```sh
curl -F 'file=@./hello.txt' \
  -F 'options={"kind":"resource"};type=application/json' \
  -H 'Idempotency-Key: hello-2026-10' \
  http://127.0.0.1:1111/api/v1/uploads
```

Send one `file` field and an optional JSON `options` field in the multipart body.
The server streams the file to disk; the client does not need to load the whole file into memory.
Without `options`, the service creates a permanent resource.
`file_name` and `file_type` override the name and MIME type from the multipart upload.

The response is `202 Accepted` with a Task body.
Poll `GET /api/v1/tasks/{id}` to check its state.
A task starts as `queued`, changes to `running`, and ends as `succeeded`, `failed`, or `cancelled`.
`cancelling` means the worker is waiting for a safe point to stop.
A native image encoder may need to finish its current step before it can stop.
FFmpeg tasks can stop their child process and remove unfinished work.
`stage`, `completed_units`, and `total_units` describe progress; `total_units` may be `null`.
They do not promise a time estimate or a progress percentage.

The `result` field depends on the task:

| Task | Result |
| --- | --- |
| Upload or media processing | A complete Media object |
| MP4 export | MP4 artifact metadata, a download path, and its expiry |
| Export | `artifact_path`, `artifact` file metadata, and `media_count` |
| Import | `archive_id`, `imported`, `skipped`, `id_map`, and `file_id_map` |

A failed task has an `error` with a stable `code` and a message for people to read.
Call `POST /api/v1/tasks/{id}/cancel` to request cancellation.
Call `POST /api/v1/tasks/{id}/retry` to retry a failed or cancelled task.
For an MP4 task from single-use video, both cancellation and retry require its still-valid `session=TOKEN` credential.
Other tasks do not need this parameter.
Successful tasks remove their temporary input and work files right away.
Finished task records, transfer archives, and input kept for retry are removed after seven days by default.
MP4 exports have a separate 24-hour retention period, and their task records stay at least until the artifact expires.
New tasks save their effective processing settings so that retries keep the same output recipe.

`Idempotency-Key` prevents duplicate tasks when a client sends the same request again.
It must contain 1 to 128 printable ASCII bytes.
Uploads, media processing, imports, and both export types support this header.
Playback-session claims also support it so that retrying the claim returns the same session.
The same key and input return the existing task; a different input with the same key returns `409`.
A reused MP4 request for single-use media still checks the supplied session.
For a repeated processing request, this also works after the source is deleted or expires.
This protection lasts only as long as the task record is kept.

## Image sizes and animations

```sh
curl -F 'file=@./photo.png' \
  -F 'options={"kind":"image","image":{"save_original":true,"variants":[{"name":"card","max_width":640,"max_height":360,"crop":{"width":16,"height":9},"multipliers":[1,2,3]},{"name":"avatar","max_width":128,"max_height":128,"crop":{"width":1,"height":1},"multipliers":[1,2]}]}};type=application/json' \
  http://127.0.0.1:1111/api/v1/uploads
```

The service applies EXIF orientation, then crops and resizes the image.
It does not make the image larger than the source allows.
The Media object's `variants` list gives each output's actual size, format, multiplier, and `content_path`.
Use these paths instead of guessing which outputs exist.
Resolve relative paths against the service root URL, including any reverse proxy prefix.
Do not add `/api/v1` again.

Each name must be unique and contain 1 to 64 ASCII letters, digits, `_`, or `-`.
The name `original` is reserved.
Multipliers must be unique, include `1`, and stay within the service limit.

WebP is the main output format.
Images with an alpha channel also get a PNG fallback; other images get JPEG.
GIF, WebP, and APNG animations produce animated WebP and GIF, plus a still PNG or JPEG of the first full frame.
Newly encoded JPEG uses progressive encoding, and newly encoded PNG and GIF use interlacing.
Trust mode can keep the matching uploaded fallback without changing its encoding.
WebP does not provide the same kind of progressive display.
APNG decoding needs an FFmpeg delegate, and frame delays are rounded to 10 ms.
GIF frames are not optimized again after conversion.
SVG and SVGZ support embedded images and internal references, but external image paths and URLs make the task fail.
SVG XML and embedded data share a 64 MiB limit, with at most 32 nested SVG images; the image pixel limits still apply.
SVG text uses installed system fonts.
SVGZ originals use `application/gzip` and keep the uploaded bytes.

The original file is kept by default.
To process a saved original into new outputs, send image, audio, or video options to `POST /api/v1/media/{id}/tasks`.
The `kind` field defaults to `image`, so existing `{"image":{...}}` requests keep their meaning.
The task creates new media and leaves the source unchanged.
Single-use media cannot be processed again and returns `409`.
If the source has an expiry time, the new result keeps the same time and is not saved if it has already expired.
Migration keeps old image outputs without converting them again.
Old crop settings that were not saved appear as `recipe: null`.
Images without an original file can still serve their existing outputs.

## Audio

```sh
curl -F 'file=@./recording.wav' \
  -F 'options={"kind":"audio","audio":{"preserve_lossless":true,"processing_mode":"trust"}};type=application/json' \
  http://127.0.0.1:1111/api/v1/uploads
```

The default AAC output uses the built-in AAC-LC encoder, 48 kHz, and `aac_coder=twoloop`.
The service first encodes 256 kbps and compares its compressed audio payload with the source over the same valid duration.
If that payload is larger, it encodes 128 kbps from the source instead.
The second output may be larger than the source.
There is no MP3 fallback.
Single-channel and stereo sources keep their channel count, while other layouts are mixed to stereo for AAC.
The service processes one audio stream, selected by `audio_stream`, then the source default, then the first audio stream.
`audio_stream` is the source stream index, not a position in a filtered list of audio streams.

`preserve_lossless` defaults to `false`.
When enabled for a compatible lossless source, it adds FLAC with compression level 12, the original sample rate, bit depth, and channel count, plus the selected AAC fallback.
It does not turn a lossy source into lossless audio.
If compatible lossless samples cannot be represented in FLAC, or a needed FLAC encoder is unavailable, the task returns AAC and a `lossless_not_preserved` warning.

`Media.audio.variants` lists the available outputs with their codec, bitrate, sample rate, channels, processing method, file metadata, and `content_path`.
Follow the returned content paths to select M4A or FLAC.
`audio.save_original` defaults to `false`.
Set it to `true` to keep a source for later reprocessing.

## Video and HLS

```sh
curl -F 'file=@./movie.mp4' \
  -F 'options={"kind":"video","video":{"variants":[{"resolution":720,"fps":30},{"resolution":1080,"fps":60}],"preserve_lossless":true}};type=application/json' \
  http://127.0.0.1:1111/api/v1/uploads
```

Every video request must supply 1 to 16 resolution and fps pairs.
There is no default ladder.
The available horizontal canvases are:

| Resolution | Canvas |
| --- | --- |
| 144p | 256×144 |
| 240p | 426×240 |
| 360p | 640×360 |
| 432p | 768×432 |
| 480p | 854×480 |
| 540p | 960×540 |
| 576p | 1024×576 |
| 720p | 1280×720 |
| 900p | 1600×900 |
| 1080p | 1920×1080 |
| 1440p | 2560×1440 |
| 2160p | 3840×2160 |

Supported fps tiers are `10`, `12`, `15`, `20`, `24`, `25`, `30`, `48`, `50`, and `60`.
Requests are adjusted downward for the source, and identical effective results are combined.
A source below 10 fps is converted to 10 fps.
The nominal frame cadence is also reported as a numerator and denominator, including rates such as 30000/1001.
Endpoint pictures may be held longer to preserve the source audio/video offset; `leading_hold_seconds` reports an initial hold when needed.

The service uses horizontal canvases for both horizontal and vertical sources.
It preserves the content aspect ratio, centers the content, and adds black borders.
It does not enlarge, stretch, or crop the content.
For example, 722×722 content uses a 1280×720 canvas after fitting the content inside it.

New video encoding uses `libx264`, `preset=veryslow`, `CRF=23`, and `yuv420p`.
It normalizes rotation and pixel aspect ratio, deinterlaces interlaced input, and converts HDR to SDR.
The H.264 level and reference-frame limit depend on the actual output requirements.
With global bitrate `B`, the 1080p/60 fps `maxrate` is `B` and `bufsize` is two seconds of that rate.
Other versions use `B × (width × height / 2073600)^0.75 × (fps tier / 60)^0.5`, rounded up to the next 50,000 bits per second, with a two-second buffer.

Normal storage is HLS VOD with fMP4 initialization files and segments.
Video and audio are separate, and allowed combinations share the same stored audio tracks.
Below 720p, a version allows the lower AAC output.
From 720p, it also allows the higher AAC output when that output passed the size rule.
From 1080p, it also allows FLAC when lossless preservation was requested and succeeded.
Video with no audio remains valid.
The audio stream and channel rules are the same as for standalone audio.
`video.save_original` defaults to `false`.

`Media.video.variants` reports the actual versions, for example `1080p30`, their exact frame rates, allowed audio identifiers, and playlist paths.
`Media.video.audio` reports the shared audio tracks.
The public Media object contains summaries, not the full segment inventory.
Use `Media.video.master_path` for the default AAC master playlist.
Public Media paths are relative to the service root, such as `api/v1/media/UUID/hls/master.m3u8`.
Inside the master playlist, child paths are relative to that playlist, such as `1080p30/index.m3u8`.
Inside a track playlist, `init.mp4` and `segment-000000.m4s` are relative to the track path.
Sequence numbers start at zero and use at least six decimal digits.
Use the playlist URLs instead of constructing file paths.
Both playlists and assets support GET and HEAD, and assets also support byte ranges.
`audio=all` includes all allowed combinations, and `audio=flac` selects FLAC combinations.
The playlist average and peak bandwidth values are measured from the segments rather than copied from the encoder limit.

The minimal [player example](../examples/player/README.md) is served at `/api/v1/player`.
It demonstrates adaptive playback and AAC/FLAC selection after checking browser support.
Clients must handle an unsupported or failing FLAC decoder by using AAC.
A change between AAC and FLAC may require rebuilding the playback source.

## Trust mode

Image, audio, and video options accept `processing_mode: "trust"`; the default is `"transcode"`.
The service checks the source requirements independently for each output and each encoded stream.
A compliant file can be reused directly, and compliant audio or video can be remuxed without encoding it again.
Outputs report `copied`, `remuxed`, or `transcoded` through `processing_method`.
Older stored image metadata may omit this field or use `unknown`.
Trust mode does not require the original encoder, preset, or quality target to match the service recipe.

Images still need a WebP main output.
A compliant uploaded JPEG or PNG can replace only its matching fallback.
For a 1000×1000 JPEG with a maximum side of 1080, the service can reuse the JPEG fallback and creates a 1000×1000 WebP.
It skips 2x and 3x because those outputs would exceed the source size.
A matching WebP can serve as the main output without another encoding pass.
Changes to crop, orientation, color, dimensions, or animation behavior can require encoding.

For audio, compliant AAC-LC and FLAC streams can be reused at their existing bitrate.
For video, the source must meet the codec, H.264 profile and level, canvas, fps, color, timestamp, bitrate, and independent segment-boundary requirements.
Average bitrate and `faststart` alone do not prove compliance.
The service checks packet information and may encode when a necessary property cannot be confirmed.
An accepted MP4 is still divided and remuxed into HLS, while `save_original` controls the separate source reference.
Bytes used by a published output remain stored even when `save_original` is `false`.

Use `GET /api/v1/capabilities` to check processing availability before offering upload or reprocessing choices.
`media.audio` and `media.video` reflect their encoder availability.
`av.available` reports external tool discovery, while `av.audio_encoder`, `av.video_encoder`, and `av.flac_encoder` report separate encoding support.
`av.minimum_tool_major` is 9, and `av.unavailable_reason` explains a failed discovery or disabled processing feature.
`video.mp4_export` can be true even without the H.264 encoder because exporting copies existing streams.
The audio/video recipe and tier fields describe configured behavior rather than promising that every encoder is installed.
Missing FFmpeg tools or a disabled `av-convert` feature do not prevent serving existing media.

## Downloads and expiry

`GET /api/v1/media/{id}` returns metadata.
`GET /api/v1/media?page=1&per_page=50` lists media.
Pages start at 1, and `per_page` can be at most 100.
A page after the last item returns an empty list.
UUIDs are strings, and dates use UTC RFC 3339 strings.
`file_size` and the page's `total` are decimal strings that a JavaScript client can read with `BigInt`.

`GET /api/v1/media/{id}/content` returns a resource, the default image output, or standalone audio.
Use the HLS paths for normal video playback.
For images, use `variant`, `multiplier`, and `format` to select an output.
Use `variant=original` for the original file, or `download=true` to send it as an attachment.
Normal content supports HEAD, SHA-256 ETags, `If-None-Match`, one byte range, `If-Range`, `206`, and `416`.
The service checks that content exists before it checks cache conditions.
`Last-Modified` is the content creation time, and `Date` is the response time.

`retention.expires_in_seconds` sets a lifetime from 1 second to 10,000 hours.
For a new upload, this period starts after processing, when the media is saved.
Downloads can be repeated until expiry.
Leave this field out to keep the media permanently.

For resources and images, `retention.single_use` allows one download claim.
Metadata and HEAD requests do not use that claim.
GET claims the content before sending the full file, so only one request can succeed.
An interrupted download does not restore the claim.
These single-use downloads ignore Range and cache conditions.

For audio and video, `retention.single_use` allows one playback-session claim instead.
Read the metadata before claiming the session, then send `POST /api/v1/media/{id}/playback-sessions`.
The response is `201 Created` with a 64-character lowercase hexadecimal `token` and `expires_at`.
Expiry uses millisecond precision from the first response, and an idempotent retry returns the same value.
Use `Idempotency-Key` when claiming so a repeated request can recover the same session.
After the claim, ordinary metadata reads and lists hide the consumed media.
Add `session=TOKEN` to content, original-file, HLS, MP4-export, and MP4-artifact requests for this media.
The token is not returned in public Media or Task results.
Authorized playlists carry the same token in their nested URLs; ordinary playlists do not echo a supplied token.
The same token can load many segments, change versions, seek, stop, and replay while it is valid.
The default lifetime is 24 hours, it does not slide on use, and it cannot exceed the media expiry time.
Deleting the media invalidates the session.
This means one claim of playback access, not a limit of one viewing.
Ordinary audio and video need no playback session.

Both expiring and single-use media use `Cache-Control: no-store`.
Authorized session downloads support byte ranges without claiming a second session.

`DELETE /api/v1/media/{id}` returns `204` when it removes the media.
Cleanup removes the file contents after no media or reader uses them.

## MP4 export

```sh
curl -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: movie-1080p30-export' \
  -d '{"variant":"1080p30"}' \
  http://127.0.0.1:1111/api/v1/media/UUID/mp4-exports
```

Select an identifier from the existing `Media.video.variants` list.
The response is `202 Accepted` with a task whose kind is `mp4_export`.
After it succeeds, follow its artifact path or use `GET /api/v1/tasks/{id}/artifact`.
The service chooses the best existing audio allowed by the video version: FLAC, then higher AAC, then lower AAC.
If the video has no audio, the export contains video only.
The task remuxes existing compressed streams and writes `faststart` without re-encoding.
To export a version that has not been produced, first create it by processing a saved original.

The result contains `media_id`, `variant`, the selected `audio` identifier or `null`, `artifact`, `artifact_path`, and `expires_at`.
Its paths are relative to the service root and do not contain a session token.
New MP4 generation requires `av-convert` and usable FFmpeg and ffprobe version 9 or later.
Existing HLS, standalone audio, playback sessions, and completed artifacts can still be read without that processing feature.

A completed MP4 expires 24 hours after completion by default, capped by the original media expiry and any required session expiry.
Its result reports the artifact expiry, and its task record is retained at least that long.
This lifetime is separate from the retention of TAR transfer archives and task history.
Normal exports take an input snapshot so deleting the source does not interrupt an accepted export, while its original media expiry still applies.
For single-use media, MP4 export and download also require the same still-valid session and media.

## Import and export

```sh
datalith --environment ./source export ./all-media.tar
datalith --environment ./source export ./selected-media.tar --id UUID
datalith --environment ./destination import ./all-media.tar
```

The CLI does not replace an existing output file.
Archive version 2 is a TAR archive with a JSON manifest, saved originals, all image and standalone audio outputs, and the complete HLS segment inventory.
Version 1 archives can still be imported.
Playback sessions and temporary MP4 exports are not part of the transfer archive.
Each unique file content is included once.
This format moves media between services; it is not a copy of the SQLite database.

For HTTP export, send `{}` to `POST /api/v1/exports` to export all media.
Send `{"ids":["UUID"]}` to export selected media.
After the task succeeds, download the archive from `GET /api/v1/tasks/{id}/artifact`.
To import, upload the TAR archive in the multipart `file` field of `POST /api/v1/imports`.

Export pauses uploads, deletes, new single-use claims, cleanup, and saving new conversion results.
Normal downloads and reads through already claimed playback sessions continue.
New write requests that cannot run during export return `503` with `Retry-After: 1`.
The archive does not include task records, expired media, or media whose single-use download was claimed.

Import checks all file contents and hashes before it saves any media.
It skips an item if its ID and data already match.
If an ID is used by different data, it assigns a new ID.
The result's `id_map` contains media ID changes, and `file_id_map` contains file ID changes.
Importing the same archive again does not create duplicates.
The CLI and HTTP API use the same stored task queue.

## Client API and errors

Open Swagger UI at `/api/v1/docs` and read the OpenAPI 3.1 JSON at `/api/v1/docs/json`.
The document is generated by utoipa from route annotations and Rust types; its entry point is [`src/rocket_mounts/openapi.rs`](src/rocket_mounts/openapi.rs).
Swagger UI resources are embedded in the binary and do not need a CDN.
The previous `/api/v1/openapi.json` path redirects to the generated document.

| Method and path, after `/api/v1` | Use |
| --- | --- |
| `POST /uploads` | Upload a file and create a task |
| `GET /tasks/{id}` | Read task state and result |
| `POST /tasks/{id}/cancel`, `POST /tasks/{id}/retry` | Cancel or retry a task |
| `GET /media`, `GET /media/{id}` | List media or read metadata |
| `POST /media/{id}/tasks` | Create new image, audio, or video media from a saved original |
| `POST /media/{id}/playback-sessions` | Claim one session for single-use audio or video |
| `GET/HEAD /media/{id}/hls/master.m3u8` | Read the AAC, all-audio, or FLAC master playlist |
| `GET/HEAD /media/{id}/hls/{track}/index.m3u8` | Read a track playlist |
| `GET/HEAD /media/{id}/hls/{track}/init.mp4` | Read an HLS initialization file |
| `GET/HEAD /media/{id}/hls/{track}/segment-{sequence}.m4s` | Read a segment, using the sequence text from its playlist |
| `POST /media/{id}/mp4-exports` | Remux an existing video version into a temporary MP4 |
| `GET/HEAD /media/{id}/content` | Download content or read its headers |
| `DELETE /media/{id}` | Delete media |
| `POST /exports`, `POST /imports` | Create a transfer task |
| `GET/HEAD /tasks/{id}/artifact` | Download a TAR or MP4 export, or read its headers |
| `GET /player` | Open the minimal browser player example |
| `GET /capabilities` | Read supported features and limits |

HTTP errors use this form: `{"error":{"code":"not_found","message":"not found"},"request_id":"UUID"}`.
Every response also has an `X-Request-Id` header.
Clients should use the HTTP status and `error.code` to handle errors, not the message text.
Common statuses are `400` and `422` for invalid input, plus `404`, `409`, `413`, `415`, `416`, `500`, and `503`.

## Build and check

Use the Rust version listed in the workspace's `rust-version`, or a later version.
The image build needs ImageMagick, WebP with webpmux, an FFmpeg delegate, and a writable temporary directory.
The tested FFmpeg 9.0.2 and ImageMagick 7.1.2-32 pairing supports APNG delegation; older ImageMagick delegates may need an upgrade for FFmpeg 9 options.
Audio and video processing also need external FFmpeg and ffprobe version 9 or later.
Docker and CI use the fixed FFmpeg build described in [FFMPEG.md](../FFMPEG.md), including licenses and corresponding sources.
Use `--no-default-features` to build without image or audio/video conversion and native MIME detection.

```sh
cargo +nightly fmt --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

## License

[MIT](LICENSE)
