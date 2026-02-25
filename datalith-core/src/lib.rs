/*!
# Datalith Core

A file management system powered by SQLite for metadata storage and the file system for file storage.

## Data Structures

* `File`: Represents a real, concrete file that is physically stored in the file system.
* `Resource`: Represents a regular file in any type. One or more `Resource` entries can point to the same `File`.
* `Image`: Represents any image in a format supported by Datalith. Each `Image` point to multiple `File`s because it includes **the original image file** as well as **thumbnails** in different resolutions and types.

## Examples

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
    // do something
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
let file = datalith.get_file_by_id(file_id).await.unwrap().unwrap(); // A temporary file can be retrieved using the `get_file_by_id` function only once. After that, it cannot be retrieved again.

// do something

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
    // do something
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
let resource = datalith.get_resource_by_id(resource_id).await.unwrap().unwrap(); // A temporary resource can be retrieved using the `get_resource_by_id` function only once. After that, it cannot be retrieved again.

// do something

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

// do something

datalith.close().await;
# }
#
# #[cfg(not(feature = "image-convert"))]
# fn main () {}
```
*/

#![cfg_attr(docsrs, feature(doc_cfg))]

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

/// The default mime type.
pub const DEFAULT_MIME_TYPE: Mime = APPLICATION_OCTET_STREAM;

/// A string of an encrypted file ID which can be used as a URL component.
pub type IDToken = String;
