#[cfg(feature = "magic")]
use std::str::FromStr;
use std::{
    io,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, TimeZone};
use mime::Mime;
#[cfg(feature = "magic")]
use once_cell::sync::Lazy;
use rand::TryRngCore;
use sha2::{Digest, Sha256};
#[cfg(feature = "magic")]
use tokio::task;
use tokio::{fs::File, io::AsyncReadExt};
use trim_in_place::TrimInPlace;

#[cfg(feature = "magic")]
use crate::magic_cookie_pool::MagicCookiePool;

/// The buffer size used when reading a file.
pub(crate) const BUFFER_SIZE: usize = 64 * 1024;

#[cfg(feature = "magic")]
static MAGIC_COOKIE: Lazy<Option<MagicCookiePool>> =
    Lazy::new(|| MagicCookiePool::new(num_cpus::get() * 2));

#[cfg(feature = "magic")]
pub(crate) async fn detect_file_type_by_buffer(file_data: impl AsRef<[u8]>) -> Option<Mime> {
    if let Some(magic_cookie) = MAGIC_COOKIE.as_ref() {
        let cookie = magic_cookie.acquire_cookie().await;

        match cookie.buffer(file_data.as_ref()) {
            Ok(result) => Mime::from_str(&result).ok(),
            Err(_) => None,
        }
    } else {
        None
    }
}

#[cfg(not(feature = "magic"))]
#[inline]
pub(crate) async fn detect_file_type_by_buffer(_file_data: impl AsRef<[u8]>) -> Option<Mime> {
    None
}

pub(crate) async fn detect_file_type_by_path(
    file_path: impl Into<PathBuf>,
    detect_using_path: bool,
) -> Option<Mime> {
    let file_path = Arc::new(file_path.into());

    #[cfg(feature = "magic")]
    if let Some(magic_cookie) = MAGIC_COOKIE.as_ref() {
        let file_path = file_path.clone();

        let result = task::spawn_blocking(move || {
            let cookie = magic_cookie.acquire_cookie_sync();

            cookie.file(file_path.as_path())
        })
        .await
        .unwrap();

        if let Ok(result) = result {
            return Mime::from_str(&result).ok();
        }
    }

    if detect_using_path {
        file_path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| mime_guess::from_ext(extension).first_or_octet_stream())
    } else {
        None
    }
}

#[inline]
pub(crate) fn get_current_timestamp() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

pub(crate) fn get_file_name<Tz: TimeZone>(
    file_name: Option<impl Into<String>>,
    date_time: DateTime<Tz>,
    mime_type: &Mime,
) -> String {
    let mut file_name = if let Some(file_name) = file_name {
        let mut file_name = file_name.into();

        file_name.trim_in_place();

        file_name
    } else {
        String::new()
    };

    let ext = get_mime_extension(mime_type);

    if file_name.is_empty() {
        if let Some(ext) = ext {
            format!("{}.{}", date_time.timestamp_millis(), ext)
        } else {
            date_time.timestamp_millis().to_string()
        }
    } else {
        if Path::new(file_name.as_str()).extension().is_none() {
            if let Some(ext) = ext {
                file_name.push('.');
                file_name.push_str(ext);
            }
        }

        file_name
    }
}

#[inline]
fn get_mime_extension(mime_type: &Mime) -> Option<&'static str> {
    match mime_type.subtype() {
        mime::JPEG => Some("jpg"),
        mime::GIF => Some("gif"),
        mime::PNG => Some("png"),
        mime::BMP => Some("bmp"),
        mime::SVG => Some("svg"),
        mime::OCTET_STREAM => Some("bin"),
        _ => match mime_type.essence_str() {
            "image/webp" => Some("webp"),
            "image/heic" => Some("heic"),
            "application/vnd.rar" | "application/x-rar" => Some("rar"),
            "application/x-iso9660-image" => Some("iso"),
            "application/x-ms-installer" | "application/x-msi" => Some("msi"),
            _ => mime_guess::get_mime_extensions(mime_type).map(|e| e[0]),
        },
    }
}

#[cfg(feature = "image-convert")]
/// Get an image extension for a given Mime.
///
/// This function allows you to generate a file name based on `image_stem` and `file_type`.
#[inline]
pub fn get_image_extension(mime_type: &Mime) -> Option<&'static str> {
    match mime_type.subtype() {
        mime::JPEG => Some("jpg"),
        mime::PNG => Some("png"),
        _ => match mime_type.essence_str() {
            "image/webp" => Some("webp"),
            _ => None,
        },
    }
}

pub(crate) async fn get_hash_by_path(file_path: impl AsRef<Path>) -> io::Result<[u8; 32]> {
    let file_path = file_path.as_ref();

    let mut file = File::open(file_path).await?;
    let expected_file_size = file.metadata().await?.len();

    let mut hasher = Sha256::new();

    let mut buffer = vec![0; calculate_buffer_size(expected_file_size)];

    loop {
        let c = file.read(&mut buffer).await?;

        if c == 0 {
            break;
        }

        hasher.update(&buffer[..c]);
    }

    Ok(hasher.finalize().into())
}

#[inline]
pub(crate) fn get_hash_by_buffer(buffer: impl AsRef<[u8]>) -> [u8; 32] {
    let buffer = buffer.as_ref();

    let mut hasher = Sha256::new();

    hasher.update(buffer);

    hasher.finalize().into()
}

#[inline]
pub(crate) fn get_random_hash() -> [u8; 32] {
    let mut rng = rand::rngs::OsRng;
    let mut data = [0u8; 32];

    rng.try_fill_bytes(&mut data).unwrap();

    data
}

#[inline]
pub(crate) fn allow_not_found_error(result: io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[inline]
pub(crate) fn calculate_buffer_size(expected_length: u64) -> usize {
    expected_length.clamp(64, BUFFER_SIZE as u64) as usize
}
