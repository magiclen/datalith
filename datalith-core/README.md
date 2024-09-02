Datalith Core
====================

A file management system powered by SQLite for metadata storage and the file system for file storage.

## Examples

#### Put a File

```rust
use datalith_core::{mime, Datalith, FileTypeLevel};
use tokio::io::AsyncReadExt;

let datalith = Datalith::new("datalith").await.unwrap();

let file = datalith.put_file_by_buffer(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap();

let mut reader = file.create_reader().await.unwrap();

let mut s = String::new();
reader.read_to_string(&mut s).await.unwrap();

println!("{s}"); // Hello world!

datalith.close().await;
```

#### Get a File

```rust
use std::str::FromStr;

use datalith_core::{uuid::Uuid, Datalith, FileTypeLevel};
use tokio::io::AsyncReadExt;

let datalith = Datalith::new("datalith").await.unwrap();

let file = datalith.get_file_by_id(Uuid::from_str("c31343fc-eae1-4416-809a-a6d96b69b3b9").unwrap()).await.unwrap();

if let Some(file) = file {
    // do something
} else {
    println!("not found");
}

datalith.close().await;
```

#### Put a Temporary File

```rust
use datalith_core::{mime, Datalith, FileTypeLevel};

let datalith = Datalith::new("datalith").await.unwrap();

let file_id = datalith.put_file_by_buffer_temporarily(b"Hello world!", Some("plain.txt"), Some((mime::TEXT_PLAIN_UTF_8, FileTypeLevel::Manual))).await.unwrap().id();
let file = datalith.get_file_by_id(file_id).await.unwrap().unwrap(); // A temporary file can be retrieved using the `get_file_by_id` function only once. After that, it cannot be retrieved again.

// do something

datalith.close().await;
```

#### Put an Image

```rust
use datalith_core::{mime, CenterCrop, Datalith};

let datalith = Datalith::new("datalith").await.unwrap();

let image = datalith.put_image_by_path("/path/to/image", Some("my-image"), Some(1280), Some(720), CenterCrop::new(16.0, 9.0), true).await.unwrap();

println!("image size: {}x{}", image.image_width(), image.image_height());

let original_file = image.original_file();
let thumbnails = image.thumbnails();                   // WebP files (1x, 2x, 3x)
let fallback_thumbnails = image.fallback_thumbnails(); // JPEG or PNG files (1x, 2x, 3x)

// do something

datalith.close().await;
```

## Crates.io

https://crates.io/crates/datalith

## Documentation

https://docs.rs/datalith

## License

[MIT](LICENSE)