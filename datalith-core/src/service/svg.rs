use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use flate2::read::GzDecoder;
use resvg::{tiny_skia, usvg};

use super::{ImageLimits, ServiceError, image_processor::check_dimensions};

const MAX_SVG_BYTES: usize = 64 * 1024 * 1024;
const MAX_SVG_DEPTH: usize = 32;

pub(super) struct RenderedSvg {
    pub path: PathBuf,
    pub mime: &'static str,
}

fn read_xml(reader: impl Read) -> Result<Option<Vec<u8>>, ServiceError> {
    let mut reader = BufReader::new(reader);
    let header = reader.fill_buf()?;
    let header = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header).trim_ascii_start();
    if !header.starts_with(b"<") {
        return Ok(None);
    }
    let mut data = Vec::new();
    reader.take(MAX_SVG_BYTES as u64 + 1).read_to_end(&mut data)?;
    if data.len() > MAX_SVG_BYTES {
        return Err(ServiceError::Invalid("SVG data exceeds 64 MiB.".into()));
    }
    Ok(Some(data))
}

fn read_svg(reader: impl Read) -> Result<(Option<Vec<u8>>, bool), ServiceError> {
    let mut reader = BufReader::new(reader);
    let compressed = reader.fill_buf()?.starts_with(&[0x1F, 0x8B]);
    let data = if compressed { read_xml(GzDecoder::new(reader))? } else { read_xml(reader)? };
    Ok((data, compressed))
}

pub(super) fn render(
    input: &Path,
    output: &Path,
    limits: &ImageLimits,
    cancel: &AtomicBool,
) -> Result<Option<RenderedSvg>, ServiceError> {
    let (data, compressed) = read_svg(File::open(input)?)?;
    let Some(data) = data else {
        return Ok(None);
    };
    let external = AtomicBool::new(false);
    let oversized = AtomicBool::new(false);
    let bytes = AtomicUsize::new(data.len());
    let depth = AtomicUsize::new(0);
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|href, _| {
                if !href.starts_with('#') {
                    external.store(true, Ordering::Relaxed);
                }
                None
            }),
            resolve_data:   Box::new(|mime, data, options| {
                if bytes.fetch_add(data.len(), Ordering::Relaxed).saturating_add(data.len())
                    > MAX_SVG_BYTES
                {
                    oversized.store(true, Ordering::Relaxed);
                    return None;
                }
                match mime {
                    "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" => {
                        default_data(mime, data, options)
                    },
                    "image/svg+xml" | "text/plain" => {
                        let nested = match read_svg(data.as_slice()) {
                            Ok((Some(data), _)) => data,
                            Ok((None, _)) => {
                                let mime = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                                    "image/png"
                                } else if data.starts_with(b"\xFF\xD8") {
                                    "image/jpeg"
                                } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")
                                {
                                    "image/gif"
                                } else if data.starts_with(b"RIFF")
                                    && data.get(8..12) == Some(b"WEBP")
                                {
                                    "image/webp"
                                } else {
                                    return None;
                                };
                                return default_data(mime, data, options);
                            },
                            Err(_) => {
                                oversized.store(true, Ordering::Relaxed);
                                return None;
                            },
                        };
                        if bytes
                            .fetch_add(nested.len(), Ordering::Relaxed)
                            .saturating_add(nested.len())
                            > MAX_SVG_BYTES
                            || depth.load(Ordering::Relaxed) >= MAX_SVG_DEPTH
                        {
                            oversized.store(true, Ordering::Relaxed);
                            return None;
                        }
                        depth.fetch_add(1, Ordering::Relaxed);
                        // Nested SVG files use the same resolver, so they cannot load files either.
                        let tree = usvg::Tree::from_data(&nested, options).ok();
                        depth.fetch_sub(1, Ordering::Relaxed);
                        tree.map(usvg::ImageKind::SVG)
                    },
                    _ => None,
                }
            }),
        },
        ..usvg::Options::default()
    };
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    options.fontdb = FONTS
        .get_or_init(|| {
            let mut fonts = usvg::fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        })
        .clone();
    let tree = usvg::Tree::from_data(&data, &options)
        .map_err(|error| ServiceError::Invalid(format!("Invalid SVG: {error}")))?;
    if external.load(Ordering::Relaxed) {
        return Err(ServiceError::Invalid(
            "SVG images cannot reference external files or URLs.".into(),
        ));
    }
    if oversized.load(Ordering::Relaxed) {
        return Err(ServiceError::Invalid(
            "Embedded SVG data exceeds the size or nesting limit.".into(),
        ));
    }
    let size = tree.size().to_int_size();
    check_dimensions(u64::from(size.width()), u64::from(size.height()), 1, limits)?;
    let mut pixels = u64::from(size.width()) * u64::from(size.height());
    check_images(tree.root(), limits, &mut pixels)?;
    if cancel.load(Ordering::Acquire) {
        return Err(ServiceError::Cancelled);
    }
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| ServiceError::Invalid("Cannot allocate the SVG image.".into()))?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    if cancel.load(Ordering::Acquire) {
        return Err(ServiceError::Cancelled);
    }
    let path = output.join("svg.png");
    pixmap
        .save_png(&path)
        .map_err(|error| ServiceError::Invalid(format!("Cannot render SVG: {error}")))?;
    Ok(Some(RenderedSvg {
        path,
        mime: if compressed { "application/gzip" } else { "image/svg+xml" },
    }))
}

fn check_images(
    group: &usvg::Group,
    limits: &ImageLimits,
    pixels: &mut u64,
) -> Result<(), ServiceError> {
    for node in group.children() {
        if let usvg::Node::Image(image) = node {
            let size = image.size().to_int_size();
            check_dimensions(u64::from(size.width()), u64::from(size.height()), 1, limits)?;
            *pixels = pixels.saturating_add(u64::from(size.width()) * u64::from(size.height()));
            if *pixels > limits.max_total_pixels {
                return Err(ServiceError::Invalid(
                    "SVG images exceed the total pixel limit.".into(),
                ));
            }
        }
        if let usvg::Node::Group(group) = node {
            check_images(group, limits, pixels)?;
        }
        let mut result = Ok(());
        node.subroots(|group| {
            if result.is_ok() {
                result = check_images(group, limits, pixels);
            }
        });
        result?;
    }
    Ok(())
}
