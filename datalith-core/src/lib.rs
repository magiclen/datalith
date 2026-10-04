/*!
# Datalith Core

Datalith Core stores file contents on disk and metadata in SQLite.
Files with the same content share one stored copy, and one process owns each data folder.

Use [`DatalithService`] to upload files, process media, and move data through background tasks.
Enable the conversion flags in [`UploadOptions`] to let the service select a matching media type.
Files that do not match an enabled type stay resources.
Tasks can recover after a restart.

## Start a service

```rust,no_run
use datalith_core::{Datalith, DatalithService, ServiceConfig, UploadOptions};

# #[tokio::main]
# async fn main() -> Result<(), Box<dyn std::error::Error>> {
let store = Datalith::new("data").await?;
let service = DatalithService::new(store, ServiceConfig::default()).await?;
let task = service.submit_upload(
    &b"Hello world!"[..],
    UploadOptions { file_name: Some("hello.txt".into()), ..UploadOptions::default() },
    None,
).await?;
println!("Task: {}", task.id);
service.close().await?;
# Ok(())
# }
```

Read the task with [`DatalithService::get_task`] to get its state and result.
Image settings use [`ImageOptions`], audio settings use [`AudioOptions`], and video settings use [`VideoOptions`].
Video conversion needs explicit resolution and frame-rate pairs.

[`Retention`] controls expiry and single-use access.
[`ProcessingMode::Trust`] can reuse content that meets the output requirements.

The older direct storage methods on [`Datalith`] remain available for existing applications.
Use `DatalithService` for new applications, and avoid mixing its writes with direct storage writes.
*/

pub extern crate chrono;
pub extern crate mime;
pub extern crate uuid;

mod datalith;
mod datalith_errors;
mod datalith_file;
mod functions;
mod guard;
#[cfg(feature = "image-convert")]
mod image;
#[cfg(feature = "magic")]
mod magic_cookie_pool;
#[cfg(feature = "manager")]
mod manager;
mod resources;
mod service;

pub use datalith::*;
pub use datalith_errors::*;
pub use datalith_file::*;
#[cfg(feature = "image-convert")]
pub use functions::get_image_extension;
#[cfg(feature = "image-convert")]
pub use image::*;
#[cfg(feature = "manager")]
pub use manager::*;
use mime::{APPLICATION_OCTET_STREAM, Mime};
pub use rdb_pagination::{OrderMethod, OrderMethodValue, Pagination, PaginationOptions};
pub use resources::*;
pub use service::*;

/// The default MIME type.
pub const DEFAULT_MIME_TYPE: Mime = APPLICATION_OCTET_STREAM;

/// An encrypted file ID for use in a URL.
pub type IDToken = String;
