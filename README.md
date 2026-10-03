# Datalith

Datalith is a small media storage service that runs as one instance.
It keeps file contents on the local file system and stores metadata and tasks in SQLite.
Files with the same SHA-256 hash share one stored copy.
It does not need a separate database or queue server.

## Features

- Upload a file, get a task ID, and poll `/api/v1` for the result.
- Keep files permanently, set an expiry time, or allow only one download.
- Crop and resize one image into several named sizes, with 1x, 2x, and 3x versions when the source is large enough.
- Serve WebP with progressive JPEG or interlaced PNG fallbacks.
- Read GIF, WebP, and APNG animations and create animated WebP, interlaced GIF, and a still fallback.
- Export all media or selected IDs to a TAR archive and import it into another Datalith service.

The data model has audio and video types for future use, but this release does not convert them.
The planned audio formats are M4A with LC-AAC and MP3 as a fallback.
The planned video format is MP4 with H.264 and AAC.
FFmpeg is currently used only by ImageMagick to read APNG animations.

See the [HTTP and CLI guide](datalith/README.md) and the [Rust API guide](datalith-core/README.md).
A running service also provides `/api/v1/openapi.json` and `/api/v1/capabilities`.
The old [Node.js client](https://github.com/magiclen/node-datalith) does not support this API.
A new client library will be built in a separate project.

## Build and run

The project uses Rust edition 2024 and requires Rust 1.94 or later.
CI checks the minimum supported Rust version (MSRV) with Rust 1.94.1.
MIME detection needs the libmagic development package.
Image support also needs ImageMagick development headers, pkg-config, and Clang.
The ImageMagick version range supported by `magick_rust` is `>= 7.1.1, < 7.2`.

```sh
cargo build --locked --release -p datalith
./target/release/datalith --environment ./data
```

For a smaller service without image conversion:

```sh
cargo build --locked --release -p datalith --no-default-features --features magic
```

Image conversion uses `image-convert` 0.23.
ImageMagick must support PNG, JPEG, WebP, and GIF.
Animated WebP needs libwebpmux, and APNG needs an ImageMagick coder and an FFmpeg delegate.
A delegate is an external program that ImageMagick calls to read or write a format.
Keep both the ImageMagick shared libraries and its configuration directory when you deploy the service.
The Docker image and CI install `imagemagick/policy.xml`, an ImageMagick security policy that allows only these formats and the FFmpeg delegate; use it for other deployments too.
APNG decoding and the image cache need a writable temporary directory.

The default image limits are 50 million pixels per frame, 500 frames, 100 million decoded pixels in total, and 16 named image settings.
The service also limits ImageMagick to 256 MiB of memory cache, 512 MiB of mapped cache, 2 GiB of disk cache, and one thread.
These limits apply to the whole process.
A lower limit from an existing ImageMagick policy stays in place.

APNG frame delays are rounded to 10 ms.
GIF frames are combined into full frames before editing and are not optimized again, so the output may be larger than the source.
WebP does not have the same progressive display mode as JPEG.
Task cancellation takes effect between processing steps.
A native encoder or delegate that is already running must finish its current step first.

## Docker

```sh
docker compose -f docker-compose.image.yml up --build -d
```

Use `docker-compose.yml` for the service without image conversion.
Both Dockerfiles use Debian Bookworm for the build and runtime stages, with Rust 1.99.0 in the builder.
The image build uses [ImageMagick 7.1.2-32](https://github.com/ImageMagick/ImageMagick/releases/tag/7.1.2-32) and checks the SHA-256 hash of its source archive.
The runtime includes FFmpeg, libwebpmux, and the ImageMagick delegate settings.

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

## Upgrade from version 0.1

Version 0.2 replaces the old HTTP API.
At startup, Datalith upgrades the old database and keeps the existing File, Resource, and Image IDs.
Before changing a nonempty version 1 database, it creates `datalith.sqlite.v1.bak`.
The backup is written to a temporary file and renamed only after it is complete.
Metadata changes are then saved in one database transaction.

Existing image outputs become variants under the `default` name and are not converted again.
Old crop settings were not stored, so their recipes stay unknown.
Images without an original file can still be downloaded, but they cannot be processed into new sizes.

Stop the old process and back up the whole environment directory before upgrading.
The automatic `.v1.bak` file contains only the database.
The new service may remove duplicate file contents, so that database file alone is not enough to restore the old environment.
If migration fails, the HTTP service does not start; restart the process to try again.
A database from a newer, unsupported version is rejected.

Tasks and their input files survive a restart.
Interrupted work is queued again.
Saving the media result and marking its task as successful happen in the same database transaction.
Successful tasks remove their temporary input and work files right away.
Failed and cancelled tasks keep their input for retry, for seven days by default.
Finished task records and exported archives use the same retention period.
Unused files are removed only after their database references and active readers are gone.
Cleanup runs again if an earlier cleanup was interrupted.

## Import and export

The CLI needs exclusive access to the environment, so stop its HTTP service before running these commands:

```sh
datalith --environment ./source export ./media.tar
datalith --environment ./destination import ./media.tar
```

Use the HTTP import and export endpoints while the service is running.
An archive contains a versioned JSON manifest and one copy of each file, including saved originals and image outputs.
It leaves out expired media, used single-use media, and unfinished uploads.
Export pauses content writes and cleanup, while normal downloads continue.
Single-use downloads are paused because they need to record the download claim.

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

CI uses `.github/scripts/install-imagemagick.sh` to install the fixed ImageMagick version on Ubuntu.
The MSRV job uses `--lib --bins` on purpose.

## License

[MIT](LICENSE)
