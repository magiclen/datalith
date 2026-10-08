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
    let mime = crate::functions::detect_file_type_by_path(input).await;
    let image = image_header(&header)
        || mime.as_ref().is_some_and(|mime| mime.type_() == crate::mime::IMAGE);
    #[cfg(feature = "image-convert")]
    {
        let input = input.to_path_buf();
        let detected = mime.as_ref().map(|mime| mime.essence_str().to_owned());
        let convert = options.enable_convert_to_image;
        let image = tokio::task::spawn_blocking(move || {
            // resvg renders SVG files, and ImageMagick is not allowed to read them.
            if detected.as_deref() == Some("image/svg+xml") || super::svg::is_svg(&input)? {
                return Ok(true);
            }
            // Without image conversion, an image only needs to skip the audio and video checks before it is stored as a resource.
            Ok::<_, ServiceError>(
                image
                    && (!convert
                        || super::image_processor::identifiable(&input, detected.as_deref())?),
            )
        })
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))??;
        if image {
            return Ok(MediaKind::Image);
        }
    }
    #[cfg(not(feature = "image-convert"))]
    if image {
        return Ok(MediaKind::Image);
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
        || bmp_header(header)
        || header.starts_with(b"8BPS")
        || header.starts_with(b"qoif")
        || header.starts_with(b"\x76\x2F\x31\x01")
        || header.starts_with(b"II\x2A\0")
        || header.starts_with(b"MM\0\x2A")
        || icon_format(header).is_some()
        || header.starts_with(b"\xFF\x0A")
        || header.starts_with(b"\0\0\0\x0CJXL \r\n\x87\n")
        || header.starts_with(b"\0\0\0\x0CjP  \r\n\x87\n")
        || (header.starts_with(b"RIFF") && header.get(8..12) == Some(b"WEBP"))
        || heif_format(header).is_some()
}

// Plain text can also start with `BM`, so check the size of the DIB header that follows, like libmagic does.
fn bmp_header(header: &[u8]) -> bool {
    header.starts_with(b"BM")
        && header.get(14..18).is_some_and(|size| {
            matches!(
                u32::from_le_bytes(size.try_into().unwrap()),
                12 | 16 | 40 | 52 | 56 | 64 | 108 | 124
            )
        })
}

// Return the ImageMagick format of an ICO or CUR file, using the rule of image-convert, which needs a nonzero image count.
pub(super) fn icon_format(header: &[u8]) -> Option<&'static str> {
    if header.get(4..6).is_none_or(|count| count == [0, 0]) {
        return None;
    }
    match header[..4] {
        [0, 0, 1, 0] => Some("ICO"),
        [0, 0, 2, 0] => Some("CUR"),
        _ => None,
    }
}

// Return the ImageMagick format of an ISO base media file whose major or compatible brands mark it as an AVIF or HEIF image.
pub(super) fn heif_format(header: &[u8]) -> Option<&'static str> {
    if header.get(4..8) != Some(b"ftyp") {
        return None;
    }
    let size = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    if size < 16 {
        return None;
    }
    let major: &[u8; 4] = header.get(8..12)?.try_into().unwrap();
    let compatible =
        header.get(16..size.min(header.len())).map_or(&[][..], |brands| brands.as_chunks::<4>().0);
    let brands = || std::iter::once(major).chain(compatible);
    if brands().any(|brand| matches!(brand, b"avif" | b"avis")) {
        Some("AVIF")
    } else if brands().any(|brand| {
        matches!(
            brand,
            b"heic"
                | b"heix"
                | b"hevc"
                | b"hevx"
                | b"heim"
                | b"heis"
                | b"hevm"
                | b"hevs"
                | b"mif1"
                | b"msf1"
        )
    }) {
        Some("HEIC")
    } else {
        None
    }
}
