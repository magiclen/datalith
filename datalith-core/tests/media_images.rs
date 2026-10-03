#![cfg(feature = "image-convert")]

use std::time::Duration;

use datalith_core::{
    ContentRequest, CropRatio, Datalith, DatalithService, ImageOptions, ImageVariantSpec, Media,
    MediaKind, ServiceConfig, TaskStatus, UploadOptions, Variant,
};
use image_convert::{
    ImageResource, InterlaceType, WEBPConfig, identify_ping, identify_read, to_webp,
};
use tokio::io::AsyncReadExt;

// These small fixtures come from the image-convert 0.23.0 test suite.
const GIF: &[u8] = include_bytes!("data/media-animation.gif");
const APNG: &[u8] = include_bytes!("data/media-animation.png");
const ORIENTED_JPEG: &[u8] = include_bytes!("data/media-orientation.jpg");

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
    let done = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let task = service.get_task(task.id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(TaskStatus::Succeeded, done.status, "{:?}", done.error);
    serde_json::from_value(done.result.unwrap()).unwrap()
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

#[tokio::test]
async fn deleting_a_legacy_image_releases_each_file_reference() {
    let directory = tempfile::tempdir().unwrap();
    let datalith = Datalith::new(directory.path()).await.unwrap();
    let mut original = ImageResource::Data(Vec::new());
    image_convert::to_png(
        &mut original,
        &ImageResource::Data(include_bytes!("data/image.png").to_vec()),
        &image_convert::PNGConfig::default(),
    )
    .unwrap();
    let image = datalith
        .put_image_by_buffer(
            original.into_vec().unwrap(),
            Some("source.png"),
            None,
            None,
            None,
            true,
        )
        .await
        .unwrap();
    let id = image.id();
    let source_id = image.original_file().unwrap().id();
    assert_eq!(source_id, image.fallback_thumbnails()[0].id());
    drop(image);
    assert!(datalith.delete_image_by_id(id).await.unwrap());
    assert!(!datalith.check_file_exist(source_id).await.unwrap());
    datalith.close().await;
}
