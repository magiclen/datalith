use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use tokio::{fs::File, io::AsyncReadExt};

use super::{DatalithService, MediaKind, ServiceError, UploadOptions};

pub(super) async fn detect(
    service: &DatalithService,
    input: &Path,
    options: &UploadOptions,
    cancel: &AtomicBool,
) -> Result<MediaKind, ServiceError> {
    if cancel.load(Ordering::Acquire) {
        return Err(ServiceError::Cancelled);
    }
    let mut header = Vec::new();
    File::open(input).await?.take(4096).read_to_end(&mut header).await?;
    let mime = crate::functions::detect_file_type_by_path(input, false).await;
    if image_header(&header) || mime.as_ref().is_some_and(|mime| mime.type_() == crate::mime::IMAGE)
    {
        return Ok(MediaKind::Image);
    }
    #[cfg(feature = "image-convert")]
    {
        let input = input.to_path_buf();
        if tokio::task::spawn_blocking(move || super::svg::is_svg(&input))
            .await
            .map_err(|error| ServiceError::Internal(error.to_string()))??
        {
            return Ok(MediaKind::Image);
        }
    }
    #[cfg(feature = "av-convert")]
    if (options.enable_convert_to_audio || options.enable_convert_to_video)
        && let Some(kind) = super::av_processor::detect_kind(service, input, cancel).await?
    {
        return Ok(kind);
    }
    let _ = (service, options);
    Ok(match mime.as_ref().map(crate::mime::Mime::type_) {
        Some(crate::mime::AUDIO) => MediaKind::Audio,
        Some(crate::mime::VIDEO) => MediaKind::Video,
        _ => MediaKind::Resource,
    })
}

fn image_header(header: &[u8]) -> bool {
    header.starts_with(b"\x89PNG\r\n\x1a\n")
        || header.starts_with(b"\xFF\xD8\xFF")
        || header.starts_with(b"GIF87a")
        || header.starts_with(b"GIF89a")
        || header.starts_with(b"\x8AMNG\r\n\x1A\n")
        || header.starts_with(b"BM")
        || header.starts_with(b"8BPS")
        || header.starts_with(b"qoif")
        || header.starts_with(b"\x76\x2F\x31\x01")
        || header.starts_with(b"II\x2A\0")
        || header.starts_with(b"MM\0\x2A")
        || header.starts_with(b"\0\0\x01\0")
        || header.starts_with(b"\0\0\x02\0")
        || header.starts_with(b"\xFF\x0A")
        || header.starts_with(b"\0\0\0\x0CJXL \r\n\x87\n")
        || header.starts_with(b"\0\0\0\x0CjP  \r\n\x87\n")
        || (header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP"))
        || image_brands(header)
}

fn image_brands(header: &[u8]) -> bool {
    if header.get(4..8) != Some(b"ftyp") {
        return false;
    }
    let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    if size < 16 {
        return false;
    }
    let brand = |brand: &[u8; 4]| {
        matches!(
            brand,
            b"avif" | b"avis" | b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
        )
    };
    let Some(major) = header.get(8..12) else {
        return false;
    };
    brand(major.try_into().unwrap())
        || header
            .get(16..size.min(header.len()))
            .is_some_and(|brands| brands.as_chunks::<4>().0.iter().any(brand))
}
