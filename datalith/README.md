# Datalith

Datalith stores file contents on disk and keeps metadata in SQLite.
Files with the same SHA-256 hash share one stored copy.
One image upload can produce several crops, sizes, multipliers, and formats.
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

Image options are available only with the `image-convert` feature.
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
`stage`, `completed_units`, and `total_units` describe progress; `total_units` may be `null`.
They do not promise a time estimate or a progress percentage.

The `result` field depends on the task:

| Task | Result |
| --- | --- |
| Upload or image processing | A complete Media object |
| Export | `artifact_path`, `artifact` file metadata, and `media_count` |
| Import | `archive_id`, `imported`, `skipped`, `id_map`, and `file_id_map` |

A failed task has an `error` with a stable `code` and a message for people to read.
Call `POST /api/v1/tasks/{id}/cancel` to request cancellation.
Call `POST /api/v1/tasks/{id}/retry` to retry a failed or cancelled task.
Successful tasks remove their temporary input and work files right away.
Finished task records, exported archives, and input kept for retry are removed after seven days by default.

`Idempotency-Key` prevents duplicate tasks when a client sends the same request again.
It must contain 1 to 128 printable ASCII bytes.
Uploads, image processing, imports, and exports support this header.
The same key and input return the existing task; a different input with the same key returns `409`.
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
JPEG uses progressive encoding, and PNG and GIF use interlacing.
WebP does not provide the same kind of progressive display.
APNG decoding needs an FFmpeg delegate, and frame delays are rounded to 10 ms.
GIF frames are not optimized again after conversion.
SVG and SVGZ support embedded images and internal references, but external image paths and URLs make the task fail.
SVG XML and embedded data share a 64 MiB limit, with at most 32 nested SVG images; the image pixel limits still apply.
SVG text uses installed system fonts.
SVGZ originals use `application/gzip` and keep the uploaded bytes.

The original file is kept by default.
To process a saved original into new outputs, send `{"image":{...}}` to `POST /api/v1/media/{id}/tasks`.
Single-use media cannot be processed again and returns `409`.
If the source has an expiry time, the new result keeps the same time and is not saved if it has already expired.
Migration keeps old image outputs without converting them again.
Old crop settings that were not saved appear as `recipe: null`.
Images without an original file can still serve their existing outputs.

Audio and video types are reserved, but this release does not convert them.
The planned audio formats are M4A with LC-AAC and MP3 as a fallback.
The planned video format is MP4 with H.264 and AAC.
Use `GET /api/v1/capabilities` to check the supported features and limits.

## Downloads and expiry

`GET /api/v1/media/{id}` returns metadata.
`GET /api/v1/media?page=1&per_page=50` lists media.
Pages start at 1, and `per_page` can be at most 100.
A page after the last item returns an empty list.
UUIDs are strings, and dates use UTC RFC 3339 strings.
`file_size` and the page's `total` are decimal strings that a JavaScript client can read with `BigInt`.

`GET /api/v1/media/{id}/content` returns a resource or the default image output.
For images, use `variant`, `multiplier`, and `format` to select an output.
Use `variant=original` for the original file, or `download=true` to send it as an attachment.
Normal content supports HEAD, SHA-256 ETags, `If-None-Match`, one byte range, `If-Range`, `206`, and `416`.
The service checks that content exists before it checks cache conditions.
`Last-Modified` is the content creation time, and `Date` is the response time.

`retention.expires_in_seconds` sets a lifetime from 1 second to 10,000 hours.
For a new upload, this period starts after processing, when the media is saved.
Downloads can be repeated until expiry.
Leave this field out to keep the media permanently.

Set `retention.single_use` to `true` to allow only one download, with or without an expiry time.
Metadata and HEAD requests do not use the download claim.
GET claims the download before sending the file, so only one request can succeed.
An interrupted download does not restore the claim.
Single-use downloads ignore Range and cache conditions and return the full file.
Both expiring and single-use content use `Cache-Control: no-store`.

`DELETE /api/v1/media/{id}` returns `204` when it removes the media.
Cleanup removes the file contents after no media or reader uses them.

## Import and export

```sh
datalith --environment ./source export ./all-media.tar
datalith --environment ./source export ./selected-media.tar --id UUID
datalith --environment ./destination import ./all-media.tar
```

The CLI does not replace an existing output file.
An export is a versioned TAR archive with a JSON manifest, saved originals, and all stored image outputs.
Each unique file content is included once.
This format moves media between services; it is not a copy of the SQLite database.

For HTTP export, send `{}` to `POST /api/v1/exports` to export all media.
Send `{"ids":["UUID"]}` to export selected media.
After the task succeeds, download the archive from `GET /api/v1/tasks/{id}/artifact`.
To import, upload the TAR archive in the multipart `file` field of `POST /api/v1/imports`.

Export pauses uploads, deletes, single-use downloads, cleanup, and saving new conversion results.
Normal downloads continue.
New write requests that cannot run during export return `503` with `Retry-After: 1`.
The archive does not include task records, expired media, or media whose single-use download was claimed.

Import checks all file contents and hashes before it saves any media.
It skips an item if its ID and data already match.
If an ID is used by different data, it assigns a new ID.
The result's `id_map` contains media ID changes, and `file_id_map` contains file ID changes.
Importing the same archive again does not create duplicates.
The CLI and HTTP API use the same stored task queue.

## Client API and errors

The full OpenAPI 3.1 document is in [`src/openapi.json`](src/openapi.json).
A running service also provides it at `GET /api/v1/openapi.json`.

| Method and path, after `/api/v1` | Use |
| --- | --- |
| `POST /uploads` | Upload a file and create a task |
| `GET /tasks/{id}` | Read task state and result |
| `POST /tasks/{id}/cancel`, `POST /tasks/{id}/retry` | Cancel or retry a task |
| `GET /media`, `GET /media/{id}` | List media or read metadata |
| `POST /media/{id}/tasks` | Create new image outputs from a saved original |
| `GET/HEAD /media/{id}/content` | Download content or read its headers |
| `DELETE /media/{id}` | Delete media |
| `POST /exports`, `POST /imports` | Create a transfer task |
| `GET/HEAD /tasks/{id}/artifact` | Download an export or read its headers |
| `GET /capabilities` | Read supported features and limits |

HTTP errors use this form: `{"error":{"code":"not_found","message":"not found"},"request_id":"UUID"}`.
Every response also has an `X-Request-Id` header.
Clients should use the HTTP status and `error.code` to handle errors, not the message text.
Common statuses are `400` and `422` for invalid input, plus `404`, `409`, `413`, `415`, `416`, `500`, and `503`.

## Build and check

Use the Rust version listed in the workspace's `rust-version`, or a later version.
The image build needs ImageMagick, WebP with webpmux, an FFmpeg delegate, and a writable temporary directory.
Use `--no-default-features` to build without image conversion or native MIME detection.

```sh
cargo +nightly fmt --all
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

## License

[MIT](LICENSE)
