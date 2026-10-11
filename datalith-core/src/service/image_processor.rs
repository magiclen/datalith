use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
};

use image_convert::{
    Crop, GIFConfig, ImageIdentify, ImageResource, JPGConfig, MagickError, PNGConfig, WEBPConfig,
    compute_output_size, fetch_magic_wand, identify_ping, identify_ping_frames, identify_read,
    magick_rust::{MagickWand, ResourceType, bindings},
    to_gif, to_jpg, to_png, to_webp,
};
use uuid::Uuid;

use super::{
    ImageLimits, ImageOptions, ImageVariantSpec, ProcessingMethod, ProcessingMode, ServiceError,
    tasks::Interruption,
};

pub(crate) struct ProcessedImage {
    pub variants:      Vec<ProcessedVariant>,
    pub animated:      bool,
    pub frame_count:   u32,
    pub original_mime: String,
}

pub(crate) struct ProcessedVariant {
    pub processing_method: ProcessingMethod,
    pub spec:              ImageVariantSpec,
    pub multiplier:        u8,
    pub format:            String,
    pub width:             u32,
    pub height:            u32,
    pub animated:          bool,
    pub path:              PathBuf,
    pub mime:              String,
}

fn image_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::Invalid(format!("Image processing failed: {error}"))
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

pub(super) fn configure_resources() -> Result<(), ServiceError> {
    static CONFIGURED: OnceLock<Result<(), String>> = OnceLock::new();
    CONFIGURED
        .get_or_init(|| {
            image_convert::start_call_once();
            // image-convert converts ICC profiles to sRGB with Little CMS and fails such images without it, even when the profile is a variant of sRGB.
            // ImageMagick returns a static string, which lives for the whole process.
            let delegates = unsafe { CStr::from_ptr(bindings::GetMagickDelegates()) };
            if !delegates.to_string_lossy().split_whitespace().any(|delegate| delegate == "lcms") {
                return Err("ImageMagick needs the lcms delegate to convert color profiles.".into());
            }
            configure_policy()?;
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

fn configure_policy() -> Result<(), String> {
    let policy =
        CString::new(include_str!("image_policy.xml")).map_err(|error| error.to_string())?;
    // The private wand is created and freed here, and the policy string lives through the call.
    let accepted = unsafe {
        let wand = bindings::NewMagickWand();
        if wand.is_null() {
            return Err("Cannot create an ImageMagick policy wand.".into());
        }
        let accepted = bindings::MagickSetSecurityPolicy(wand, policy.as_ptr());
        bindings::DestroyMagickWand(wand);
        accepted == bindings::MagickBooleanType::MagickTrue
    };
    if !accepted {
        return Err("ImageMagick rejected the image security policy.".into());
    }
    for coder in [
        c"PS", c"EPS", c"PDF", c"PCL", c"XPS", c"MSL", c"TEXT", c"HTTP", c"HTTPS", c"SVG", c"SVGZ",
        c"MSVG", c"RSVG", c"DOT", c"GV", c"MVG",
    ] {
        // ImageMagick is initialized, and each name is a static C string.
        let allowed = unsafe {
            bindings::IsRightsAuthorized(
                bindings::PolicyDomain::Coder,
                bindings::PolicyRights::Read,
                coder.as_ptr(),
            )
        };
        if allowed != bindings::MagickBooleanType::MagickFalse {
            return Err(format!("ImageMagick did not block {}.", coder.to_string_lossy()));
        }
    }
    Ok(())
}

// ImageMagick checks the length of an image list when a reader sets the size of a frame, which also limits readers that cannot stop at a frame range, such as the JPEG XL reader.
// The limit is global, so it only grows to fit the largest frame limit, and one extra frame lets the frame limit of Datalith report the image instead.
fn raise_list_length(max_frames: u32) -> Result<(), ServiceError> {
    // The first value is the limit which ImageMagick had before, such as one from the environment or the policy.
    static LIMITS: Mutex<Option<(u64, u64)>> = Mutex::new(None);

    let mut limits = LIMITS.lock().unwrap_or_else(PoisonError::into_inner);
    let (initial, current) = *limits
        .get_or_insert_with(|| (MagickWand::get_resource_limit(ResourceType::ListLength), 0));
    let limit = initial.min(u64::from(max_frames) + 2);
    if limit > current {
        MagickWand::set_resource_limit(ResourceType::ListLength, limit)
            .map_err(|error| ServiceError::Internal(error.to_string()))?;
        *limits = Some((initial, limit));
    }
    Ok(())
}

pub(super) fn check_dimensions(
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

// ImageMagick hands PostScript, EPS, PDF, and PCL to Ghostscript, which runs the file like a program, so reject them before ImageMagick reads anything.
// The checks follow the format detection of ImageMagick; other formats are left to ImageMagick.
fn reject_ghostscript_formats(path: &Path) -> Result<(), ServiceError> {
    let mut header = Vec::with_capacity(5);
    File::open(path)?.take(5).read_to_end(&mut header)?;
    let postscript = header.starts_with(b"%!") || header.starts_with(b"\x04%!");
    let eps_with_preview = header.starts_with(b"\xC5\xD0\xD3\xC6");
    let pdf = header.get(..5).is_some_and(|magic| magic.eq_ignore_ascii_case(b"%PDF-"));
    let pcl = header.starts_with(b"\x1BE\x1B") && !header.starts_with(b"\x1BE\x1B&");
    if postscript || eps_with_preview || pdf || pcl {
        Err(ServiceError::Unsupported(
            "PostScript, PDF, and other formats that need Ghostscript are not supported.".into(),
        ))
    } else {
        Ok(())
    }
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

// ImageMagick pings every frame of an animated WebP and cannot stop at a frame range, so check the canvas and the frame count from the container first.
fn check_webp_animation(path: &Path, limits: &ImageLimits) -> Result<(), ServiceError> {
    let mut file = BufReader::new(File::open(path)?);
    let length = file.get_ref().metadata()?.len();
    let mut header = [0; 30];
    // The extended format starts with the VP8X chunk, whose flags mark an animation.
    if file.read_exact(&mut header).is_err()
        || &header[..4] != b"RIFF"
        || &header[8..16] != b"WEBPVP8X"
        || header[20] & 0x02 == 0
    {
        return Ok(());
    }
    // Like libwebp, accept a VP8X chunk which is longer than its 10 bytes of fields.
    let vp8x = u32::from_le_bytes(header[16..20].try_into().unwrap());
    if vp8x < 10 {
        return Ok(());
    }
    let skip = i64::from(vp8x) + i64::from(vp8x % 2) - 10;
    file.seek_relative(skip)?;
    let mut offset = 30 + skip.unsigned_abs();
    let end = (u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8).min(length);
    let mut frames = 0;
    // Each ANMF chunk holds one frame, and one more than the limit is enough to reject the image.
    while offset + 8 <= end && frames <= u64::from(limits.max_frames) {
        let mut chunk = [0; 8];
        file.read_exact(&mut chunk)?;
        if &chunk[..4] == b"ANMF" {
            frames += 1;
        }
        let size = u32::from_le_bytes(chunk[4..].try_into().unwrap());
        let skip = i64::from(size) + i64::from(size % 2);
        file.seek_relative(skip)?;
        offset += 8 + skip.unsigned_abs();
    }
    if frames > 0 {
        // The canvas width and height are stored minus one in 24 bits.
        let dimension =
            |bytes: &[u8]| u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])) + 1;
        check_dimensions(dimension(&header[24..27]), dimension(&header[27..30]), frames, limits)?;
    }
    Ok(())
}

// The largest total size of the color profiles which coalescing gives the frames of an animation.
const MAX_ANIMATION_PROFILE_BYTES: u64 = 64 * 1024 * 1024;

// Coalescing copies the profiles of a frame to the frames after it which have none.
// Outputs only need the color profiles, so the other profiles are removed first, and the copies of the color profiles are limited.
fn keep_color_profiles(wand: &MagickWand) -> Result<(), ServiceError> {
    let mut current = 0;
    let mut total = 0u64;
    wand.reset_iterator();
    while wand.next_image() {
        wand.profile_image("!icc,!icm,*", None::<&[u8]>).map_err(image_error)?;
        // The blob writer changes the iterator, so only the current frame is cloned.
        let frame = MagickWand::new_from_image(&wand.get_image().map_err(image_error)?)
            .map_err(image_error)?;
        if let Ok(profile) = frame.write_image_blob("ICC") {
            current = profile.len() as u64;
        }
        total = total.saturating_add(current);
    }
    wand.reset_iterator();
    if total > MAX_ANIMATION_PROFILE_BYTES {
        return Err(ServiceError::Invalid(
            "The color profiles of the animation are too large.".into(),
        ));
    }
    Ok(())
}

// Ping one frame more than the limit, which shows whether an image has too many frames.
fn ping_frames(input: &ImageResource, limits: &ImageLimits) -> Result<ImageIdentify, MagickError> {
    identify_ping_frames(
        input,
        usize::try_from(limits.max_frames).unwrap_or(usize::MAX).saturating_add(1),
    )
}

// ImageMagick has no signature for these formats, and it only checks the major brand of HEIF files, so they need an explicit format.
fn explicit_format(
    path: &Path,
    detected_mime: Option<&str>,
) -> Result<Option<&'static str>, ServiceError> {
    let mut header = Vec::with_capacity(4096);
    File::open(path)?.take(4096).read_to_end(&mut header)?;
    Ok(super::classification::icon_format(&header)
        .or_else(|| super::classification::heif_format(&header))
        .or_else(|| (detected_mime == Some("image/x-tga")).then_some("TGA")))
}

fn image_resource(path: &Path, format: Option<&str>) -> Result<ImageResource, ServiceError> {
    let path = path
        .to_str()
        .ok_or_else(|| ServiceError::Invalid("The image path is not UTF-8.".into()))?;
    Ok(ImageResource::Path(match format {
        Some(format) => format!("{format}:{path}"),
        None => path.to_owned(),
    }))
}

/// Check whether ImageMagick can identify an image file, without decoding its pixels.
/// libmagic also reports formats such as CAD drawings as images, and automatic conversion keeps them as resources.
pub(super) fn identifiable(
    input: &Path,
    detected_mime: Option<&str>,
    limits: &ImageLimits,
) -> Result<bool, ServiceError> {
    match reject_ghostscript_formats(input) {
        Ok(()) => (),
        Err(ServiceError::Unsupported(_)) => return Ok(false),
        Err(error) => return Err(error),
    }
    configure_resources()?;
    raise_list_length(limits.max_frames)?;
    check_webp_animation(input, limits)?;
    let input = image_resource(input, explicit_format(input, detected_mime)?)?;
    Ok(ping_frames(&input, limits).is_ok())
}

/// Process an image file.
/// `detected_mime` is the type that libmagic detected for `input`, if any.
pub(super) fn process_image(
    input: &Path,
    output_dir: &Path,
    options: &ImageOptions,
    limits: &ImageLimits,
    interruption: &Interruption,
    detected_mime: Option<&str>,
) -> Result<ProcessedImage, ServiceError> {
    validate_options(options, limits)?;
    interruption.check()?;
    reject_ghostscript_formats(input)?;
    configure_resources()?;
    raise_list_length(limits.max_frames)?;
    let svg = super::svg::render(input, output_dir, limits, interruption)?;
    let source = svg.as_ref().map_or(input, |svg| svg.path.as_path());
    let format = if svg.is_some() { None } else { explicit_format(source, detected_mime)? };
    check_webp_animation(source, limits)?;
    let input = image_resource(source, format)?;
    let ping = ping_frames(&input, limits).map_err(image_error)?;
    // The resource path can start with a format, so read the file itself.
    let preflight_frames =
        apng_frame_count(source)?.map(u64::from).unwrap_or(ping.number_of_frames as u64);
    check_dimensions(
        u64::from(ping.resolution.width),
        u64::from(ping.resolution.height),
        preflight_frames,
        limits,
    )?;
    interruption.check()?;
    let mut decoded = None;
    let metadata = identify_read(&mut decoded, &input).map_err(image_error)?;
    let mut wand = decoded
        .ok_or_else(|| ServiceError::Internal("The image decoder returned no image.".into()))?;
    let frame_count = u32::try_from(metadata.number_of_frames).map_err(image_error)?;
    let animated = frame_count > 1
        && matches!(metadata.format.as_str(), "GIF" | "WEBP" | "PNG" | "APNG" | "MNG");
    let original_mime = if let Some(svg) = &svg {
        svg.mime.into()
    } else if let Some(mime) = detected_mime.filter(|mime| *mime != "application/octet-stream") {
        // libmagic knows the registered types, such as `image/vnd.adobe.photoshop` for PSD.
        mime.into()
    } else {
        match ping.format.as_str() {
            "JPG" | "JPEG" => "image/jpeg".into(),
            "APNG" | "PNG" => "image/png".into(),
            "SVG" => "image/svg+xml".into(),
            "ICO" | "ICON" | "CUR" => "image/vnd.microsoft.icon".into(),
            other => format!("image/{}", other.to_ascii_lowercase()),
        }
    };
    let original_path = source.to_path_buf();
    let source_format = match ping.format.as_str() {
        "JPG" | "JPEG" => "jpeg",
        "PNG" => "png",
        "WEBP" => "webp",
        "GIF" => "gif",
        _ => "",
    };
    let mut can_reuse = options.processing_mode == ProcessingMode::Trust && svg.is_none();
    wand.reset_iterator();
    let mut max_width = 0;
    let mut max_height = 0;
    while wand.next_image() {
        can_reuse &= matches!(
            wand.get_image_orientation(),
            image_convert::magick_rust::OrientationType::Undefined
                | image_convert::magick_rust::OrientationType::TopLeft
        ) && matches!(
            wand.get_image_colorspace(),
            image_convert::magick_rust::ColorspaceType::sRGB
                | image_convert::magick_rust::ColorspaceType::GRAY
        ) && (source_format != "jpeg" || wand.get_image_depth() <= 8);
        let (page_width, page_height, x, y) = wand.get_image_page();
        max_width =
            max_width.max(page_width.max(wand.get_image_width().saturating_add(x.max(0) as usize)));
        max_height = max_height
            .max(page_height.max(wand.get_image_height().saturating_add(y.max(0) as usize)));
    }
    wand.reset_iterator();
    check_dimensions(max_width as u64, max_height as u64, u64::from(frame_count), limits)?;
    interruption.check()?;
    if animated {
        keep_color_profiles(&wand)?;
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
        interruption.check()?;
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
                interruption.check()?;
                if can_reuse
                    && (!animated || matches!(*format, "webp" | "gif"))
                    && spec.crop.is_none()
                    && width == ping.resolution.width
                    && height == ping.resolution.height
                    && *format == source_format
                {
                    variants.push(ProcessedVariant {
                        processing_method: ProcessingMethod::Copied,
                        spec: spec.clone(),
                        multiplier,
                        format: (*format).into(),
                        width,
                        height,
                        animated,
                        path: original_path.clone(),
                        mime: original_mime.clone(),
                    });
                    continue;
                }
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
                    processing_method: ProcessingMethod::Transcoded,
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
    interruption.check()?;
    Ok(ProcessedImage {
        variants,
        animated,
        frame_count: if animated { frame_count } else { 1 },
        original_mime,
    })
}
