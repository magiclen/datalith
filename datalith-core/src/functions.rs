#[cfg(feature = "magic")]
use std::str::FromStr;
use std::{io, io::ErrorKind, path::Path};

use mime::Mime;
#[cfg(feature = "magic")]
use once_cell::sync::Lazy;
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
};

#[cfg(feature = "magic")]
use crate::magic_cookie_pool::MagicCookiePool;

/// The buffer size used when reading a file.
const BUFFER_SIZE: usize = 4096;

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
    file_path: impl AsRef<Path>,
    detect_using_path: bool,
) -> Option<Mime> {
    let file_path = file_path.as_ref();

    #[cfg(feature = "magic")]
    if let Some(magic_cookie) = MAGIC_COOKIE.as_ref() {
        let cookie = magic_cookie.acquire_cookie().await;

        if let Ok(result) = cookie.file(file_path) {
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

pub(crate) async fn get_hash_by_path(file_path: impl AsRef<Path>) -> io::Result<[u8; 32]> {
    let file_path = file_path.as_ref();

    let mut file = File::open(file_path).await?;

    let mut hasher = Sha256::new();

    let mut buffer = [0; BUFFER_SIZE];

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

pub(crate) async fn get_file_size_by_reader_and_copy_to_file(
    mut reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
) -> io::Result<u64> {
    let file_path = file_path.as_ref();

    let mut file = File::create(file_path).await?;

    let mut buffer = [0; BUFFER_SIZE];
    let mut file_size = 0u64;

    // copy the data and calculate the hash value
    let mut retry_count = 0;
    loop {
        let c = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(c) => c,
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                retry_count += 1;

                if retry_count > 5 {
                    return Err(error);
                }

                continue;
            },
            Err(error) => {
                fs::remove_file(file_path).await?;
                return Err(error);
            },
        };

        match file.write_all(&buffer[..c]).await {
            Ok(_) => (),
            Err(error) => {
                fs::remove_file(file_path).await?;
                return Err(error);
            },
        }

        file_size += c as u64;

        retry_count = 0;
    }

    Ok(file_size)
}

pub(crate) async fn get_file_size_and_hash_by_reader_and_copy_to_file(
    mut reader: impl AsyncRead + Unpin,
    file_path: impl AsRef<Path>,
) -> io::Result<(u64, [u8; 32])> {
    let file_path = file_path.as_ref();

    let mut hasher = Sha256::new();
    let mut file = File::create(file_path).await?;

    let mut buffer = [0; BUFFER_SIZE];
    let mut file_size = 0u64;

    // copy the data and calculate the hash value
    let mut retry_count = 0;
    loop {
        let c = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(c) => c,
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                retry_count += 1;

                if retry_count > 5 {
                    return Err(error);
                }

                continue;
            },
            Err(error) => {
                fs::remove_file(file_path).await?;
                return Err(error);
            },
        };

        match file.write_all(&buffer[..c]).await {
            Ok(_) => (),
            Err(error) => {
                fs::remove_file(file_path).await?;
                return Err(error);
            },
        }

        hasher.update(&buffer[..c]);

        file_size += c as u64;

        retry_count = 0;
    }

    Ok((file_size, hasher.finalize().into()))
}

#[inline]
pub(crate) fn get_random_hash() -> [u8; 32] {
    let mut rng = rand::thread_rng();
    let mut data = [0u8; 32];

    rng.fill(&mut data);

    data
}
