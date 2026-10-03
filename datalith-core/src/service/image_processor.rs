use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use image_convert::{
    Crop, GIFConfig, ImageResource, JPGConfig, PNGConfig, WEBPConfig, compute_output_size,
    fetch_magic_wand, identify_ping, identify_read,
    magick_rust::{MagickWand, ResourceType},
    to_gif, to_jpg, to_png, to_webp,
};
use uuid::Uuid;

use super::{ImageLimits, ImageOptions, ImageVariantSpec, ServiceError};

pub(crate) struct ProcessedImage {
    pub variants:      Vec<ProcessedVariant>,
    pub animated:      bool,
    pub frame_count:   u32,
    pub original_mime: String,
}

pub(crate) struct ProcessedVariant {
    pub spec:       ImageVariantSpec,
    pub multiplier: u8,
    pub format:     String,
    pub width:      u32,
    pub height:     u32,
    pub animated:   bool,
    pub path:       PathBuf,
    pub mime:       String,
}

fn image_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::Invalid(format!("Image processing failed: {error}"))
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), ServiceError> {
    if cancel.load(Ordering::Acquire) { Err(ServiceError::Cancelled) } else { Ok(()) }
}

pub(crate) fn validate_options(
    options: &ImageOptions,
    limits: &ImageLimits,
) -> Result<(), ServiceError> {
    if options.variants.is_empty() || options.variants.len() > limits.max_variants {
        return Err(ServiceError::Invalid(
            "The number of image variants is outside the limit.".into(),
        ));
    }
    let mut names = HashSet::new();
    for spec in &options.variants {
        if spec.name.is_empty()
            || spec.name.len() > 64
            || spec.name == "original"
            || !spec
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !names.insert(&spec.name)
        {
            return Err(ServiceError::Invalid(
                "Variant names must be unique ASCII names of 1 to 64 characters; original is \
                 reserved."
                    .into(),
            ));
        }
        if spec.max_width == Some(0) || spec.max_height == Some(0) {
            return Err(ServiceError::Invalid("Image dimensions must be positive.".into()));
        }
        if let Some(crop) = &spec.crop {
            let ratio = crop.width / crop.height;
            if !crop.width.is_finite()
                || !crop.height.is_finite()
                || crop.width <= 0.0
                || crop.height <= 0.0
                || !ratio.is_finite()
                || ratio <= 0.0
            {
                return Err(ServiceError::Invalid(
                    "Crop dimensions must form a finite positive ratio.".into(),
                ));
            }
        }
        let mut multipliers = HashSet::new();
        if spec.multipliers.is_empty()
            || !spec.multipliers.contains(&1)
            || spec.multipliers.iter().any(|value| {
                *value == 0 || *value > limits.max_multiplier || !multipliers.insert(value)
            })
        {
            return Err(ServiceError::Invalid(
                "Image multipliers must be unique, include 1, and fit the configured limit.".into(),
            ));
        }
    }
    Ok(())
}

fn configure_resources() -> Result<(), ServiceError> {
    static CONFIGURED: OnceLock<Result<(), String>> = OnceLock::new();
    CONFIGURED
        .get_or_init(|| {
            image_convert::start_call_once();
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            for (kind, ceiling) in [
                (ResourceType::Memory, 256 * 1024 * 1024),
                (ResourceType::Map, 512 * 1024 * 1024),
                (ResourceType::Disk, 2 * 1024 * 1024 * 1024),
                (ResourceType::Thread, 1),
            ] {
                let limit = MagickWand::get_resource_limit(kind).min(ceiling);
                MagickWand::set_resource_limit(kind, limit).map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .as_ref()
        .map_err(|error| ServiceError::Internal(error.clone()))
        .copied()
}

fn check_dimensions(
    width: u64,
    height: u64,
    frames: u64,
    limits: &ImageLimits,
) -> Result<(), ServiceError> {
    let pixels = width.saturating_mul(height);
    let total = pixels.saturating_mul(frames);
    if width == 0
        || height == 0
        || frames == 0
        || pixels > limits.max_pixels
        || frames > u64::from(limits.max_frames)
        || total > limits.max_total_pixels
    {
        return Err(ServiceError::Invalid("The image exceeds the pixel or frame limit.".into()));
    }
    Ok(())
}

// Read the APNG frame count before the decoder creates frames.
fn apng_frame_count(path: &Path) -> Result<Option<u32>, ServiceError> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut signature = [0; 8];
    if file.read_exact(&mut signature).is_err() || signature != *b"\x89PNG\r\n\x1a\n" {
        return Ok(None);
    }
    loop {
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(image_error)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap());
        let end = file
            .stream_position()?
            .checked_add(u64::from(size) + 4)
            .ok_or_else(|| ServiceError::Invalid("Invalid PNG chunk length.".into()))?;
        if end > length {
            return Err(ServiceError::Invalid("Incomplete PNG chunk.".into()));
        }
        if &header[4..] == b"acTL" {
            if size != 8 {
                return Err(ServiceError::Invalid("Invalid APNG control chunk.".into()));
            }
            let mut count = [0; 4];
            file.read_exact(&mut count)?;
            return Ok(Some(u32::from_be_bytes(count)));
        }
        if matches!(&header[4..], b"IDAT" | b"IEND") {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(end))?;
    }
}

pub(crate) fn process_image(
    input: &Path,
    output_dir: &Path,
    options: &ImageOptions,
    limits: &ImageLimits,
    cancel: &AtomicBool,
) -> Result<ProcessedImage, ServiceError> {
    validate_options(options, limits)?;
    check_cancel(cancel)?;
    configure_resources()?;
    let input = ImageResource::Path(
        input
            .to_str()
            .ok_or_else(|| ServiceError::Invalid("The image path is not UTF-8.".into()))?
            .to_owned(),
    );
    let ping = identify_ping(&input).map_err(image_error)?;
    let preflight_frames = apng_frame_count(input.as_path().unwrap())?
        .map(u64::from)
        .unwrap_or(ping.number_of_frames as u64);
    check_dimensions(
        u64::from(ping.resolution.width),
        u64::from(ping.resolution.height),
        preflight_frames,
        limits,
    )?;
    check_cancel(cancel)?;
    let mut decoded = None;
    let metadata = identify_read(&mut decoded, &input).map_err(image_error)?;
    let mut wand = decoded
        .ok_or_else(|| ServiceError::Internal("The image decoder returned no image.".into()))?;
    let frame_count = u32::try_from(metadata.number_of_frames).map_err(image_error)?;
    let animated = frame_count > 1
        && matches!(metadata.format.as_str(), "GIF" | "WEBP" | "PNG" | "APNG" | "MNG");
    let original_mime = match ping.format.as_str() {
        "JPG" | "JPEG" => "image/jpeg".into(),
        "APNG" | "PNG" => "image/png".into(),
        "SVG" => "image/svg+xml".into(),
        "ICO" | "ICON" | "CUR" => "image/vnd.microsoft.icon".into(),
        other => format!("image/{}", other.to_ascii_lowercase()),
    };
    wand.reset_iterator();
    let mut max_width = 0;
    let mut max_height = 0;
    while wand.next_image() {
        let (page_width, page_height, x, y) = wand.get_image_page();
        max_width =
            max_width.max(page_width.max(wand.get_image_width().saturating_add(x.max(0) as usize)));
        max_height = max_height
            .max(page_height.max(wand.get_image_height().saturating_add(y.max(0) as usize)));
    }
    wand.reset_iterator();
    check_dimensions(max_width as u64, max_height as u64, u64::from(frame_count), limits)?;
    check_cancel(cancel)?;
    if animated {
        wand = wand.coalesce().map_err(image_error)?;
        wand.reset_iterator();
    }
    let decoded = ImageResource::MagickWand(wand);
    let (oriented, _) = if animated {
        fetch_magic_wand(&decoded, &WEBPConfig::default())
    } else {
        fetch_magic_wand(&decoded, &PNGConfig::default())
    }
    .map_err(image_error)?;
    drop(decoded);
    let source = ImageResource::MagickWand(oriented);
    fs::create_dir_all(output_dir)?;
    let mut variants = Vec::new();

    for spec in &options.variants {
        check_cancel(cancel)?;
        let crop = spec.crop.as_ref().map(|crop| Crop::Center(crop.width, crop.height));
        let (cropped, _) = fetch_magic_wand(&source, &WEBPConfig {
            crop,
            ..WEBPConfig::default()
        })
        .map_err(image_error)?;
        let source_width = u32::try_from(cropped.get_image_width()).map_err(image_error)?;
        let source_height = u32::try_from(cropped.get_image_height()).map_err(image_error)?;
        let has_alpha = cropped.get_image_alpha_channel();
        let cropped = ImageResource::MagickWand(cropped);
        let (base_width, base_height) = compute_output_size(
            true,
            source_width,
            source_height,
            spec.max_width.unwrap_or(0),
            spec.max_height.unwrap_or(0),
        )
        .unwrap_or((source_width, source_height));
        let mut multipliers = spec.multipliers.clone();
        multipliers.sort_unstable();
        for multiplier in multipliers {
            let Some(width) = base_width.checked_mul(u32::from(multiplier)) else {
                continue;
            };
            let Some(height) = base_height.checked_mul(u32::from(multiplier)) else {
                continue;
            };
            if width > source_width || height > source_height {
                continue;
            }
            let fallback = if has_alpha { "png" } else { "jpeg" };
            let formats: &[&str] =
                if animated { &["webp", "gif", fallback] } else { &["webp", fallback] };
            for format in formats {
                check_cancel(cancel)?;
                let path = output_dir.join(format!("{}.{}", Uuid::new_v4(), format));
                let mut output = ImageResource::from_path(&path);
                match *format {
                    "webp" => to_webp(&mut output, &cropped, &WEBPConfig {
                        width,
                        height,
                        quality: 80,
                        ..WEBPConfig::default()
                    }),
                    "gif" => to_gif(&mut output, &cropped, &GIFConfig {
                        width,
                        height,
                        ..GIFConfig::default()
                    }),
                    "png" => to_png(&mut output, &cropped, &PNGConfig {
                        width,
                        height,
                        ..PNGConfig::default()
                    }),
                    "jpeg" => to_jpg(&mut output, &cropped, &JPGConfig {
                        width,
                        height,
                        quality: Some(70),
                        ..JPGConfig::default()
                    }),
                    _ => unreachable!(),
                }
                .map_err(image_error)?;
                let actual = identify_ping(&output).map_err(image_error)?;
                variants.push(ProcessedVariant {
                    spec: spec.clone(),
                    multiplier,
                    format: (*format).into(),
                    width: actual.resolution.width,
                    height: actual.resolution.height,
                    animated: actual.number_of_frames > 1,
                    path,
                    mime: format!("image/{format}"),
                });
            }
        }
    }
    check_cancel(cancel)?;
    Ok(ProcessedImage {
        variants,
        animated,
        frame_count: if animated { frame_count } else { 1 },
        original_mime,
    })
}
