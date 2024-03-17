use std::{io, path::Path};

use mime::Mime;
use sha2::{Digest, Sha256};
use tokio::{fs::File, io::AsyncReadExt};

use crate::DEFAULT_MIME_TYPE;

const BUFFER_SIZE: usize = 4096;

#[inline]
pub(crate) fn get_mime_by_path(file_path: impl AsRef<Path>) -> Mime {
    match file_path.as_ref().extension().and_then(|extension| extension.to_str()) {
        Some(extension) => mime_guess::from_ext(extension).first_or_octet_stream(),
        None => DEFAULT_MIME_TYPE,
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

pub(crate) fn get_hash_by_buffer(buffer: impl AsRef<[u8]>) -> [u8; 32] {
    let buffer = buffer.as_ref();

    let mut hasher = Sha256::new();

    hasher.update(buffer);

    hasher.finalize().into()
}
