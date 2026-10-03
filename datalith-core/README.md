# Datalith Core

Datalith Core is the Rust library used by the Datalith service.
It stores file contents on disk and keeps metadata and tasks in SQLite.
Files with the same content share one stored copy.
Each environment directory is opened by one process at a time.

## Service API

Use `DatalithService` for the task API.
`Media` describes a stored resource or image.
`Variant` describes an image output with a name, size multiplier, and format.
`Task` holds the state and result of work that can continue after a restart.

`submit_upload` saves the upload before it returns.
Image conversion runs on a background blocking worker, so the call does not wait for conversion to finish.

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
A still image also gets interlaced PNG when it has an alpha channel, or progressive JPEG otherwise.
GIF, WebP, and APNG animations get animated WebP and interlaced GIF, plus a still PNG or JPEG of the first full frame.
The original file is kept by default.

`submit_process` creates new media from a saved original and leaves the source media available.
It rejects single-use sources.
If the source has an expiry time, the new media keeps that same time.
Processing does not extend its lifetime, and a result that has already expired is not saved.
An image without an original cannot be processed into new sizes.

Each image worker owns its MagickWand and does not share it with other threads.
`ImageLimits` controls pixels per frame, frame count, total decoded pixels, and the number of named settings.
See the [build and deployment guide](../README.md) for native libraries, resource limits, and animation timing limits.

## Read and delete media

`get_media` and `list_media` return metadata.
`open_content` returns a `Content` value with a Tokio file and a read guard.
The guard prevents the stored file from being deleted while the `Content` value is alive.
Use `ContentRequest` to select a name, multiplier, and format.
Use `variant: Some("original".into())` to read the original file.
For images, the default is the first name's 1x WebP output.

`Retention` keeps media forever by default.
Set `expires_in_seconds` to allow repeated downloads until expiry.
For a new upload, this period starts when processing is complete and the media is saved.
Set `single_use` to allow only one download claim.
You can use both settings together.

Reading metadata or calling `open_content(..., true)` for HEAD does not use that claim.
A normal content open claims the single-use download before it returns the file.
Only one caller can claim it, even when requests arrive at the same time.
An interrupted download does not restore the claim.

`delete_media` removes the media and its file references.
A file stays on disk while other media or active readers still use it.
Cleanup removes the file after all references and readers are gone.
`close` stops and waits for workers before closing the database.

## Tasks and archives

Task states are `queued`, `running`, `cancelling`, `succeeded`, `failed`, and `cancelled`.
`cancel_task` asks the worker to stop between processing steps.
`retry_task` retries a failed or cancelled task while its input is still available.
Successful tasks remove their temporary input and work files right away.
Failed and cancelled tasks keep their input for retry, for seven days by default.
Finished task records and exported archives use the same retention period.
On restart, the service reads SQLite and queues unfinished work again.

`submit_export(ExportOptions { ids: None }, ...)` exports all available media.
Pass a list of IDs to export only those items.
Use `open_artifact` to read the finished archive.
`submit_import` reads and checks an archive, then merges its media into the current environment.
If an ID is already used by different data, the import gives it a new ID and returns the mapping.
The archive stores media and file contents, not a copy of the SQLite database.

`MediaKind::Audio` and `MediaKind::Video` are reserved for future use.
Submitting either type currently returns `ServiceError::Unsupported`.
Check `capabilities()` before a client selects media types, formats, or limits.

## Older storage API

`DatalithFile`, `DatalithResource`, `DatalithImage`, and the direct storage methods are still available.
New applications should use `DatalithService` throughout, without mixing it with direct writes through the older API.
The first migration turns existing resources and images into media with the same IDs.
Files that are not part of a resource or image also keep their IDs.
Migration does not rebuild thumbnails or guess old crop settings.

## Build features

Rust 1.94 or later is required, and CI checks Rust 1.94.1.
The default features are `magic`, `image-convert`, and `manager`.
`manager` provides the older cleanup scheduler; the new service task queue does not need it.
Disable default features to build without native image or MIME libraries.

## Links

[Crates.io](https://crates.io/crates/datalith-core) | [API documentation](https://docs.rs/datalith-core)

## License

[MIT](LICENSE)
