#![cfg(feature = "image-convert")]

use std::time::Duration;

use datalith_core::{
    ContentRequest, CropRatio, Datalith, DatalithService, ImageOptions, ImageVariantSpec, Media,
    MediaKind, ServiceConfig, TaskStatus, UploadOptions, Variant,
};
use image_convert::{
    Color, ImageResource, InterlaceType, WEBPConfig, identify_ping, identify_read,
    magick_rust::{MagickWand, PixelWand},
    to_webp,
};
use tokio::io::AsyncReadExt;

// These small fixtures come from the image-convert 0.23.0 test suite.
const GIF: &[u8] = include_bytes!("data/media-animation.gif");
const APNG: &[u8] = include_bytes!("data/media-animation.png");
const ORIENTED_JPEG: &[u8] = include_bytes!("data/media-orientation.jpg");
// This profile comes from the image-convert 0.24.0 test suite and was made with Little CMS 2.
const DISPLAY_P3: &[u8] = include_bytes!("data/display_p3.icc");
// A color in Display P3, and the same color converted to sRGB, as image-convert checks them.
const DISPLAY_P3_PIXEL: [u8; 3] = [180, 100, 50];
const SRGB_PIXEL: [u8; 3] = [193, 95, 34];

async fn finished(service: &DatalithService, id: datalith_core::Uuid) -> datalith_core::Task {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let task = service.get_task(id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn upload(service: &DatalithService, bytes: &[u8], image: ImageOptions) -> Media {
    let task = service
        .submit_upload(
            bytes,
            UploadOptions {
                kind: MediaKind::Image,
                file_name: Some("example".into()),
                image,
                ..UploadOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    let done = finished(service, task.id).await;
    assert_eq!(TaskStatus::Succeeded, done.status, "{:?}", done.error);
    serde_json::from_value(done.result.unwrap()).unwrap()
}

#[tokio::test]
async fn automatic_images_keep_default_sizes_and_other_files_stay_resources() {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(svg).unwrap();
    let compressed = gzip.finish().unwrap();
    let mut first_task = None;
    let options = UploadOptions {
        enable_convert_to_image: true,
        file_name: Some("wrong-name.mp3".into()),
        file_type: Some("audio/mpeg".into()),
        ..UploadOptions::default()
    };
    for (index, (bytes, kind, animated)) in [
        (include_bytes!("data/image.png").as_slice(), MediaKind::Image, false),
        (GIF, MediaKind::Image, true),
        (svg.as_slice(), MediaKind::Image, false),
        (compressed.as_slice(), MediaKind::Image, false),
        (b"A plain file.".as_slice(), MediaKind::Resource, false),
    ]
    .into_iter()
    .enumerate()
    {
        let submitted = service
            .submit_upload(bytes, options.clone(), Some(format!("auto-image-{index}")))
            .await
            .unwrap();
        assert_eq!("upload", submitted.kind);
        first_task.get_or_insert(submitted.id);
        let done = finished(&service, submitted.id).await;
        assert_eq!(TaskStatus::Succeeded, done.status, "{:?}", done.error);
        let media: Media = serde_json::from_value(done.result.unwrap()).unwrap();
        assert_eq!(kind, media.kind);
        assert_eq!(animated, media.animated);
        if kind == MediaKind::Image {
            let variant = media.variants.iter().find(|variant| variant.format == "webp").unwrap();
            let expected = if bytes == svg || bytes == compressed {
                (20, 10)
            } else {
                let original = identify_ping(&ImageResource::Data(bytes.to_vec())).unwrap();
                (original.resolution.width, original.resolution.height)
            };
            assert_eq!(expected, (variant.width, variant.height));
            assert!(media.variants.iter().all(|variant| variant.multiplier == 1));
        } else {
            assert!(media.variants.is_empty());
        }
    }
    service.close().await.unwrap();
    drop(service);
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let repeated = service
        .submit_upload(
            include_bytes!("data/image.png").as_slice(),
            options,
            Some("auto-image-0".into()),
        )
        .await
        .unwrap();
    assert_eq!(first_task.unwrap(), repeated.id);
    assert_eq!(TaskStatus::Succeeded, repeated.status);
    service.close().await.unwrap();
}

#[tokio::test]
async fn svg_keeps_embedded_images_and_blocks_external_images() {
    use std::io::Write;

    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="10"><defs><linearGradient id="paint"><stop stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs><rect width="20" height="10" fill="url(#paint)"/><image x="10" width="10" height="10" xlink:href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAADklEQVR4nGNg+A+FMAYAQ84H+fei4u8AAAAASUVORK5CYII="/></svg>"##;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(svg).unwrap();
    let compressed = gzip.finish().unwrap();
    for (input, mime) in
        [(svg.as_slice(), "image/svg+xml"), (compressed.as_slice(), "application/gzip")]
    {
        let media = upload(&service, input, ImageOptions::default()).await;
        assert_eq!(mime, media.original.as_ref().unwrap().file_type);
        let png = media.variants.iter().find(|variant| variant.format == "png").unwrap();
        assert_eq!((20, 10), (png.width, png.height));
        let bytes = variant_bytes(&service, &media, png).await;
        let pixels = resvg::tiny_skia::Pixmap::decode_png(&bytes).unwrap();
        let pixel = pixels.pixel(15, 5).unwrap();
        assert_eq!((0, 255, 0, 255), (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()));
        let metadata = identify_ping(&ImageResource::Data(bytes)).unwrap();
        assert_eq!((20, 10), (metadata.resolution.width, metadata.resolution.height));
    }
    let canary = directory.path().join("canary.png");
    tokio::fs::write(&canary, include_bytes!("data/image.png")).await.unwrap();
    for href in [canary.to_str().unwrap(), "https://example.invalid/private.png"] {
        let external = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="10"><image width="20" height="10" xlink:href="{href}"/></svg>"#
        );
        let task = service
            .submit_upload(
                external.as_bytes(),
                UploadOptions {
                    kind: MediaKind::Image,
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let done = finished(&service, task.id).await;
        assert_eq!(TaskStatus::Failed, done.status);
        assert_eq!(
            "SVG images cannot reference external files or URLs.",
            done.error.unwrap().message
        );
        assert!(service.get_media(task.id).await.unwrap().is_none());
        let encoded: String = external.bytes().map(|byte| format!("%{byte:02X}")).collect();
        let nested = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="10"><image width="20" height="10" xlink:href="data:image/svg+xml,{encoded}"/></svg>"#
        );
        let task = service
            .submit_upload(
                nested.as_bytes(),
                UploadOptions {
                    kind: MediaKind::Image,
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let done = finished(&service, task.id).await;
        assert_eq!(TaskStatus::Failed, done.status);
        assert_eq!(
            "SVG images cannot reference external files or URLs.",
            done.error.unwrap().message
        );
    }
    // Native SVG decoding must stay blocked even when ImageMagick ignores external policy files.
    let wand = image_convert::magick_rust::MagickWand::new();
    assert!(wand.read_image_blob(svg).is_err());
    service.close().await.unwrap();
}

#[tokio::test]
async fn svg_text_uses_installed_fonts_for_generic_families() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    for family in ["serif", "sans-serif", "monospace"] {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="30"><rect width="80" height="30" fill="white"/><text x="4" y="22" font-family="{family}" font-size="20" fill="black">Hello</text></svg>"#
        );
        let media = upload(&service, svg.as_bytes(), ImageOptions::default()).await;
        let png = media.variants.iter().find(|variant| variant.format == "png").unwrap();
        let bytes = variant_bytes(&service, &media, png).await;
        let pixels = resvg::tiny_skia::Pixmap::decode_png(&bytes).unwrap();
        let dark = pixels.pixels().iter().filter(|pixel| pixel.red() < 128).count();
        assert!(dark > 0, "{family} text was not rendered");
    }
    service.close().await.unwrap();
}

// Create a 16x16 image in one color, tagged with the Display P3 profile without converting it.
fn display_p3_image(format: &str) -> Vec<u8> {
    let [r, g, b] = DISPLAY_P3_PIXEL;
    let mut color = PixelWand::new();
    color.set_color(Color::Rgba(r, g, b, 255).to_magick_color().as_ref()).unwrap();
    let mut image = MagickWand::new();
    image.new_image(16, 16, &color).unwrap();
    image.set_image_depth(8).unwrap();
    image.profile_image("icc", DISPLAY_P3).unwrap();
    image.write_image_blob(format).unwrap()
}

// Return the first pixel of an encoded image, and whether the image still has an ICC profile.
fn first_pixel(bytes: &[u8]) -> ([u8; 3], bool) {
    let image = MagickWand::new();
    image.read_image_blob(bytes).unwrap();
    let pixel = image.export_image_pixels(0, 0, 1, 1, "RGB").unwrap().try_into().unwrap();
    let profiled =
        MagickWand::new_from_image(&image.get_image().unwrap()).unwrap().write_image_blob("ICC");
    (pixel, profiled.is_ok())
}

fn assert_pixel(expected: [u8; 3], actual: [u8; 3], tolerance: u8) {
    for (expected, actual) in expected.iter().zip(actual) {
        assert!(expected.abs_diff(actual) <= tolerance, "Expected {expected:?}, got {actual:?}.");
    }
}

async fn original_bytes(service: &DatalithService, media: &Media) -> Vec<u8> {
    let mut content = service
        .open_content(
            media.id,
            ContentRequest {
                variant: Some("original".into()),
                ..ContentRequest::default()
            },
            false,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    bytes
}

#[tokio::test]
async fn display_p3_colors_are_converted_to_srgb() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let source = display_p3_image("PNG");
    let media = upload(&service, &source, ImageOptions::default()).await;
    for variant in &media.variants {
        let (pixel, profiled) = first_pixel(&variant_bytes(&service, &media, variant).await);
        // WebP is lossy, so its colors can move a little more.
        assert_pixel(SRGB_PIXEL, pixel, if variant.format == "webp" { 8 } else { 2 });
        assert!(!profiled, "{} keeps an ICC profile", variant.format);
    }
    assert_eq!(source, original_bytes(&service, &media).await);
    service.close().await.unwrap();
}

#[tokio::test]
async fn trust_copies_a_display_p3_jpeg_and_converts_its_webp() {
    use datalith_core::{ProcessingMethod, ProcessingMode};

    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let source = display_p3_image("JPEG");
    let media = upload(&service, &source, ImageOptions {
        processing_mode: ProcessingMode::Trust,
        save_original: false,
        ..ImageOptions::default()
    })
    .await;
    // The copied file keeps its profile, so color-managed viewers still show the right colors.
    let jpeg = media.variants.iter().find(|variant| variant.format == "jpeg").unwrap();
    assert_eq!(ProcessingMethod::Copied, jpeg.processing_method);
    assert_eq!(source, variant_bytes(&service, &media, jpeg).await);
    let webp = media.variants.iter().find(|variant| variant.format == "webp").unwrap();
    assert_eq!(ProcessingMethod::Transcoded, webp.processing_method);
    let (pixel, profiled) = first_pixel(&variant_bytes(&service, &media, webp).await);
    assert_pixel(SRGB_PIXEL, pixel, 8);
    assert!(!profiled);
    service.close().await.unwrap();
}

async fn variant_bytes(service: &DatalithService, media: &Media, variant: &Variant) -> Vec<u8> {
    let mut content = service
        .open_content(
            media.id,
            ContentRequest {
                variant:    Some(variant.name.clone()),
                multiplier: Some(variant.multiplier),
                format:     Some(variant.format.clone()),
            },
            false,
        )
        .await
        .unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    bytes
}

#[tokio::test]
async fn named_variants_respect_orientation_and_source_dimensions() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let image = ImageOptions {
        variants: vec![
            ImageVariantSpec {
                name: "portrait".into(),
                max_height: Some(50),
                ..ImageVariantSpec::default()
            },
            ImageVariantSpec {
                name: "square".into(),
                max_width: Some(40),
                crop: Some(CropRatio {
                    width: 1.0, height: 1.0
                }),
                ..ImageVariantSpec::default()
            },
        ],
        ..ImageOptions::default()
    };
    let media = upload(&service, ORIENTED_JPEG, image).await;
    assert!(!media.animated);
    assert_eq!(1, media.frame_count);
    assert!(media.original.is_some());
    assert_eq!(10, media.variants.len());
    for variant in &media.variants {
        let multiplier = u32::from(variant.multiplier);
        let expected = if variant.name == "portrait" {
            (25 * multiplier, 50 * multiplier)
        } else {
            (40 * multiplier, 40 * multiplier)
        };
        assert_eq!(expected, (variant.width, variant.height));
        assert!(variant.width <= 100);
        assert!(variant.height <= 200);
        let bytes = variant_bytes(&service, &media, variant).await;
        let metadata = identify_ping(&ImageResource::Data(bytes)).unwrap();
        assert_eq!(expected, (metadata.resolution.width, metadata.resolution.height));
        if variant.format == "jpeg" {
            assert_eq!(InterlaceType::JPEG, metadata.interlace);
        }
    }
    service.close().await.unwrap();
}

#[tokio::test]
async fn formats_from_image_delegates_become_images() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    // These 16x16 fixtures need the heic, jxl, jp2, and openexr delegates of ImageMagick.
    for (name, bytes, file_type) in [
        ("image.heic", include_bytes!("data/image.heic").as_slice(), "image/heic"),
        ("image.avif", include_bytes!("data/image.avif").as_slice(), "image/avif"),
        ("image.jp2", include_bytes!("data/image.jp2").as_slice(), "image/jp2"),
        ("image.exr", include_bytes!("data/image.exr").as_slice(), "image/x-exr"),
        ("image.jxl", include_bytes!("data/image.jxl").as_slice(), "image/jxl"),
    ] {
        let task = service
            .submit_upload(
                bytes,
                UploadOptions {
                    enable_convert_to_image: true,
                    file_name: Some(name.into()),
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let done = finished(&service, task.id).await;
        assert_eq!(TaskStatus::Succeeded, done.status, "{name}: {:?}", done.error);
        let media: Media = serde_json::from_value(done.result.unwrap()).unwrap();
        assert_eq!(MediaKind::Image, media.kind, "{name}");
        assert_eq!(file_type, media.original.as_ref().unwrap().file_type, "{name}");
        let webp = media.variants.iter().find(|variant| variant.format == "webp").unwrap();
        assert_eq!((16, 16), (webp.width, webp.height), "{name}");
    }
    service.close().await.unwrap();
}

#[tokio::test]
async fn icons_and_targa_images_need_no_file_extension() {
    use image_convert::{ICOConfig, to_ico};

    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let png = include_bytes!("data/image.png");
    let mut ico = ImageResource::Data(Vec::new());
    to_ico(&mut ico, &ImageResource::Data(png.to_vec()), &ICOConfig {
        size: vec![(16, 16), (32, 32), (64, 64)],
        ..ICOConfig::new()
    })
    .unwrap();
    let ico = ico.as_u8_slice().unwrap().to_vec();
    // A cursor file only differs from an icon file in its type field.
    let mut cur = ico.clone();
    cur[2] = 2;
    let wand = MagickWand::new();
    wand.read_image_blob(png).unwrap();
    let tga = wand.write_image_blob("TGA").unwrap();
    for (name, bytes, file_type, size) in [
        ("icon.ico", ico, "image/vnd.microsoft.icon", 64),
        ("cursor.cur", cur, "image/x-win-bitmap", 64),
        ("image.tga", tga, "image/x-tga", 128),
    ] {
        let task = service
            .submit_upload(
                bytes.as_slice(),
                UploadOptions {
                    kind: MediaKind::Image,
                    file_name: Some(name.into()),
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let done = finished(&service, task.id).await;
        assert_eq!(TaskStatus::Succeeded, done.status, "{name}: {:?}", done.error);
        let media: Media = serde_json::from_value(done.result.unwrap()).unwrap();
        assert_eq!(file_type, media.original.unwrap().file_type, "{name}");
        assert!(!media.animated, "{name}");
        // Icons keep their largest image.
        let webp = media
            .variants
            .iter()
            .find(|variant| variant.format == "webp" && variant.multiplier == 1)
            .unwrap();
        assert_eq!((size, size), (webp.width, webp.height), "{name}");
    }
    service.close().await.unwrap();
}

#[tokio::test]
async fn variant_file_names_drop_only_a_real_extension() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    for (file_name, expected) in
        [("photo.png", "photo-default@1x.webp"), ("x.é/.", "x.é/.-default@1x.webp")]
    {
        let task = service
            .submit_upload(
                include_bytes!("data/image.png").as_slice(),
                UploadOptions {
                    kind: MediaKind::Image,
                    file_name: Some(file_name.into()),
                    ..UploadOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        let done = finished(&service, task.id).await;
        assert_eq!(TaskStatus::Succeeded, done.status, "{:?}", done.error);
        let media: Media = serde_json::from_value(done.result.unwrap()).unwrap();
        let variant = media
            .variants
            .iter()
            .find(|variant| variant.multiplier == 1 && variant.format == "webp")
            .unwrap();
        assert_eq!(expected, variant.file.file_name);
    }
    service.close().await.unwrap();
}

#[tokio::test]
async fn trust_reuses_matching_image_files_and_still_provides_webp() {
    use datalith_core::{ProcessingMethod, ProcessingMode};
    use image_convert::{JPGConfig, to_jpg};
    use sha2::{Digest, Sha256};

    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let mut jpeg = ImageResource::Data(Vec::new());
    let mut background = image_convert::magick_rust::PixelWand::new();
    background.set_color("white").unwrap();
    let square = image_convert::magick_rust::MagickWand::new();
    square.new_image(1000, 1000, &background).unwrap();
    to_jpg(&mut jpeg, &ImageResource::MagickWand(square), &JPGConfig::default()).unwrap();
    let source = jpeg.as_u8_slice().unwrap();
    let media = upload(&service, source, ImageOptions {
        processing_mode: ProcessingMode::Trust,
        save_original:   false,
        variants:        vec![ImageVariantSpec {
            max_width: Some(1080),
            max_height: Some(1080),
            ..ImageVariantSpec::default()
        }],
    })
    .await;
    assert!(media.original.is_none());
    assert_eq!(2, media.variants.len());
    let copied = media.variants.iter().find(|variant| variant.format == "jpeg").unwrap();
    assert_eq!(ProcessingMethod::Copied, copied.processing_method);
    assert_eq!((1000, 1000, 1), (copied.width, copied.height, copied.multiplier));
    assert_eq!(hex::encode(Sha256::digest(source)), copied.file.sha256);
    assert_eq!(source, variant_bytes(&service, &media, copied).await);
    assert_eq!(
        ProcessingMethod::Transcoded,
        media.variants.iter().find(|variant| variant.format == "webp").unwrap().processing_method
    );

    let mut webp = ImageResource::Data(Vec::new());
    to_webp(&mut webp, &jpeg, &WEBPConfig::default()).unwrap();
    let source = webp.as_u8_slice().unwrap();
    let media = upload(&service, source, ImageOptions {
        processing_mode: ProcessingMode::Trust,
        save_original: false,
        ..ImageOptions::default()
    })
    .await;
    let copied = media.variants.iter().find(|variant| variant.format == "webp").unwrap();
    assert_eq!(ProcessingMethod::Copied, copied.processing_method);
    assert_eq!(hex::encode(Sha256::digest(source)), copied.file.sha256);
    assert_eq!(source, variant_bytes(&service, &media, copied).await);
    let media = upload(&service, ORIENTED_JPEG, ImageOptions {
        processing_mode: ProcessingMode::Trust,
        ..ImageOptions::default()
    })
    .await;
    assert!(
        media
            .variants
            .iter()
            .all(|variant| variant.processing_method == ProcessingMethod::Transcoded)
    );
    service.close().await.unwrap();
}

#[tokio::test]
async fn gif_webp_and_apng_keep_animation_with_crops_and_fallbacks() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let mut webp = ImageResource::Data(Vec::new());
    to_webp(&mut webp, &ImageResource::Data(GIF.to_vec()), &WEBPConfig::default()).unwrap();
    for (input, timed_apng) in [(GIF, false), (webp.as_u8_slice().unwrap(), false), (APNG, true)] {
        let image = ImageOptions {
            variants: vec![
                ImageVariantSpec {
                    name: "square".into(),
                    max_width: Some(16),
                    crop: Some(CropRatio {
                        width: 1.0, height: 1.0
                    }),
                    ..ImageVariantSpec::default()
                },
                ImageVariantSpec {
                    name: "banner".into(),
                    max_width: Some(24),
                    crop: Some(CropRatio {
                        width: 2.0, height: 1.0
                    }),
                    ..ImageVariantSpec::default()
                },
            ],
            ..ImageOptions::default()
        };
        let media = upload(&service, input, image).await;
        assert!(media.animated);
        assert_eq!(4, media.frame_count);
        assert_eq!(18, media.variants.len());
        for variant in &media.variants {
            let multiplier = u32::from(variant.multiplier);
            let expected = if variant.name == "square" {
                (16 * multiplier, 16 * multiplier)
            } else {
                (24 * multiplier, 12 * multiplier)
            };
            assert_eq!(expected, (variant.width, variant.height));
            let bytes = variant_bytes(&service, &media, variant).await;
            let mut decoded = None;
            let metadata = identify_read(&mut decoded, &ImageResource::Data(bytes)).unwrap();
            assert_eq!(expected, (metadata.resolution.width, metadata.resolution.height));
            if matches!(variant.format.as_str(), "webp" | "gif") {
                assert!(variant.animated);
                assert_eq!(4, metadata.number_of_frames);
                if timed_apng {
                    let wand = decoded.unwrap();
                    assert_eq!(3, wand.get_image_iterations());
                    wand.reset_iterator();
                    let mut delays = Vec::new();
                    while wand.next_image() {
                        delays.push(wand.get_image_delay());
                    }
                    assert_eq!(vec![10, 20, 30, 40], delays);
                }
            } else {
                assert!(!variant.animated);
                assert_eq!(1, metadata.number_of_frames);
                if variant.format == "png" {
                    let frame = decoded.unwrap();
                    let pixel = frame.export_image_pixels(0, 0, 1, 1, "RGBA").unwrap();
                    assert_eq!(255, pixel[3]);
                    assert!(pixel[0] <= 8 && pixel[1] <= 8 && pixel[2].abs_diff(128) <= 8);
                }
            }
            match variant.format.as_str() {
                "png" => assert_eq!(InterlaceType::PNG, metadata.interlace),
                "jpeg" => assert_eq!(InterlaceType::JPEG, metadata.interlace),
                "gif" => assert_eq!(InterlaceType::GIF, metadata.interlace),
                _ => (),
            }
        }
    }
    service.close().await.unwrap();
}
