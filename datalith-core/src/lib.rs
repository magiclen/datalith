/*!
# Datalith Core

A Rust library that stores file contents on disk and metadata in SQLite.

Use `DatalithService` to upload media, create image outputs, and import or export archives through stored tasks.
Tasks can recover after a restart, and files with the same content share one stored copy.
The direct storage API below is still available for older applications.

## Data Structures

* `File`: A stored file and its metadata.
* `Resource`: A file of any type.
  Several resources can refer to the same file.
* `Image`: An image with an optional original file and thumbnails in several sizes and formats.

## Direct storage examples

#### Put a File

```rust,no_run
use datalith_core::{mime, Datalith, FileTypeLevel};
use tokio::io::AsyncReadExt;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let file = datalith.put_file_by_buffer(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap();

let mut reader = file.create_reader().await.unwrap();

let mut s = String::new();
reader.read_to_string(&mut s).await.unwrap();

println!("{s}"); // Hello world!

datalith.close().await;
# }
```

#### Get a File

```rust,no_run
use std::str::FromStr;

use datalith_core::{uuid::Uuid, Datalith, FileTypeLevel};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let file = datalith.get_file_by_id(Uuid::from_str("c31343fc-eae1-4416-809a-a6d96b69b3b9").unwrap()).await.unwrap();

if let Some(file) = file {
    // Use the result here.
} else {
    println!("not found");
}

datalith.close().await;
# }
```

#### Put a Temporary File

```rust,no_run
use datalith_core::{mime, Datalith, FileTypeLevel};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let file_id = datalith.put_file_by_buffer_temporarily(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap().id();
let file = datalith.get_file_by_id(file_id).await.unwrap().unwrap(); // A temporary file can be claimed only once.

// Use the result here.

datalith.close().await;
# }
```

#### Put a Resource

```rust,no_run
use datalith_core::{mime, Datalith, FileTypeLevel};
use tokio::io::AsyncReadExt;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let resource = datalith.put_resource_by_buffer(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap();

let mut reader = resource.file().create_reader().await.unwrap();

let mut s = String::new();
reader.read_to_string(&mut s).await.unwrap();

println!("{s}"); // Hello world!

datalith.close().await;
# }
```

#### Get a Resource

```rust,no_run
use std::str::FromStr;

use datalith_core::{uuid::Uuid, Datalith, FileTypeLevel};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let resource = datalith.get_resource_by_id(Uuid::from_str("c31343fc-eae1-4416-809a-a6d96b69b3b9").unwrap()).await.unwrap();

if let Some(resource) = resource {
    // Use the result here.
} else {
    println!("not found");
}

datalith.close().await;
# }
```

#### Put a Temporary Resource

```rust,no_run
use datalith_core::{mime, Datalith, FileTypeLevel};

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let resource_id = datalith.put_resource_by_buffer_temporarily(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap().id();
let resource = datalith.get_resource_by_id(resource_id).await.unwrap().unwrap(); // A temporary resource can be claimed only once.

// Use the result here.

datalith.close().await;
# }
```

#### Put an Image

```rust,no_run
# #[cfg(feature = "image-convert")]
use datalith_core::{mime, CenterCrop, Datalith};

# #[cfg(feature = "image-convert")]
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let datalith = Datalith::new("datalith").await.unwrap();

let image = datalith.put_image_by_path("/path/to/image", Some("my-image"), Some(1280), Some(720), CenterCrop::new(16.0, 9.0), true).await.unwrap();

println!("image size: {}x{}", image.image_width(), image.image_height());

let original_file = image.original_file();
let thumbnails = image.thumbnails();                   // WebP files (1x, 2x, 3x)
let fallback_thumbnails = image.fallback_thumbnails(); // JPEG or PNG files (1x, 2x, 3x)

// Use the result here.

datalith.close().await;
# }
#
# #[cfg(not(feature = "image-convert"))]
# fn main () {}
```
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
