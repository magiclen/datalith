use std::{
    io,
    io::ErrorKind,
    path::{Path, PathBuf},
};
#[cfg(feature = "magic")]
use std::{str::FromStr, sync::LazyLock};

use mime::Mime;
use rand::TryRng;
use sha2::{Digest, Sha256};
#[cfg(feature = "magic")]
use tokio::task;
use tokio::{fs::File, io::AsyncReadExt};

#[cfg(feature = "magic")]
use crate::magic_cookie_pool::MagicCookiePool;

const BUFFER_SIZE: usize = 64 * 1024;

#[cfg(feature = "magic")]
static MAGIC_COOKIE: LazyLock<Option<MagicCookiePool>> = LazyLock::new(|| {
    let parallelism = std::thread::available_parallelism().map_or(1, |count| count.get());
    MagicCookiePool::new(parallelism * 2)
});

pub(crate) async fn detect_file_type_by_path(file_path: impl Into<PathBuf>) -> Option<Mime> {
    #[cfg(feature = "magic")]
    if let Some(magic_cookie) = MAGIC_COOKIE.as_ref() {
        let file_path = file_path.into();
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
    #[cfg(not(feature = "magic"))]
    let _ = file_path;
    None
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
pub(crate) fn get_random_hash() -> [u8; 32] {
    let mut rng = rand::rngs::SysRng;
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
