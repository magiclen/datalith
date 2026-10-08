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

pub(super) fn is_svg(input: &Path) -> Result<bool, ServiceError> {
    let mut reader = BufReader::new(File::open(input)?);
    let prefix = reader.fill_buf()?;
    let compressed = prefix.starts_with(&[0x1F, 0x8B]);
    if !compressed
        && !prefix
            .strip_prefix(b"\xEF\xBB\xBF")
            .unwrap_or(prefix)
            .trim_ascii_start()
            .starts_with(b"<")
    {
        return Ok(false);
    }
    let mut header = Vec::new();
    if compressed {
        if GzDecoder::new(reader).take(65536).read_to_end(&mut header).is_err() {
            return Ok(false);
        }
    } else {
        reader.take(65536).read_to_end(&mut header)?;
    }
    let mut header = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&header).trim_ascii_start();
    loop {
        let end = if header.starts_with(b"<!DOCTYPE") {
            doctype_end(header)
        } else {
            let ending = if header.starts_with(b"<?") {
                b"?>".as_slice()
            } else if header.starts_with(b"<!--") {
                b"-->".as_slice()
            } else {
                break;
            };
            header
                .windows(ending.len())
                .position(|part| part == ending)
                .map(|end| end + ending.len())
        };
        let Some(end) = end else {
            return Ok(false);
        };
        header = header[end..].trim_ascii_start();
    }
    let Some(header) = header.strip_prefix(b"<") else {
        return Ok(false);
    };
    let Some(end) =
        header.iter().position(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
    else {
        return Ok(false);
    };
    Ok(header[..end].rsplit(|byte| *byte == b':').next() == Some(b"svg".as_slice()))
}

// A document type can contain quoted text and an internal subset with its own tags, so it ends at the first `>` outside them.
fn doctype_end(data: &[u8]) -> Option<usize> {
    let mut quote = None;
    let mut depth = 0usize;
    data.iter()
        .position(|&byte| {
            if let Some(current) = quote {
                if byte == current {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b'[' => depth += 1,
                    b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => return true,
                    _ => (),
                }
            }
            false
        })
        .map(|end| end + 1)
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
            use_installed_generic_families(&mut fonts);
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
    let canvas = u64::from(size.width()) * u64::from(size.height());
    let mut pixels = canvas;
    check_images(tree.root(), limits, canvas, &mut pixels)?;
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

// Generic families can name fonts which are not installed, and usvg drops text without a font, so point them at installed fonts.
fn use_installed_generic_families(fonts: &mut usvg::fontdb::Database) {
    use usvg::fontdb::{Database, Family, Query};

    fn installed(fonts: &Database, family: Family) -> bool {
        fonts
            .query(&Query {
                families: &[family],
                ..Query::default()
            })
            .is_some()
    }

    let fallback =
        fonts.faces().find_map(|face| face.families.first()).map(|(name, _)| name.clone());
    for (generic, preferred) in [
        (Family::Serif, "DejaVu Serif"),
        (Family::SansSerif, "DejaVu Sans"),
        (Family::Monospace, "DejaVu Sans Mono"),
    ] {
        if installed(fonts, generic) {
            continue;
        }
        let name = if installed(fonts, Family::Name(preferred)) {
            Some(preferred.to_owned())
        } else {
            fallback.clone()
        };
        let Some(name) = name else {
            continue;
        };
        match generic {
            Family::Serif => fonts.set_serif_family(name),
            Family::SansSerif => fonts.set_sans_serif_family(name),
            _ => fonts.set_monospace_family(name),
        }
    }
}

fn check_images(
    group: &usvg::Group,
    limits: &ImageLimits,
    canvas: u64,
    pixels: &mut u64,
) -> Result<(), ServiceError> {
    for node in group.children() {
        if let usvg::Node::Image(image) = node {
            let size = image.size().to_int_size();
            check_dimensions(u64::from(size.width()), u64::from(size.height()), 1, limits)?;
            // resvg draws each nested SVG on a new pixmap as large as the whole canvas.
            let cost = if matches!(image.kind(), usvg::ImageKind::SVG(_)) {
                canvas
            } else {
                u64::from(size.width()) * u64::from(size.height())
            };
            *pixels = pixels.saturating_add(cost);
            if *pixels > limits.max_total_pixels {
                return Err(ServiceError::Invalid(
                    "SVG images exceed the total pixel limit.".into(),
                ));
            }
        }
        if let usvg::Node::Group(group) = node {
            check_images(group, limits, canvas, pixels)?;
        }
        let mut result = Ok(());
        node.subroots(|group| {
            if result.is_ok() {
                result = check_images(group, limits, canvas, pixels);
            }
        });
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn svg_detection_skips_a_document_type_with_an_internal_subset() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(
            br#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
    <!ENTITY ns_extend "http://ns.adobe.com/Extensibility/1.0/">
    <!ENTITY ns_ai "http://ns.adobe.com/AdobeIllustrator/10.0/">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>
"#,
        )
        .unwrap();
        assert!(is_svg(file.path()).unwrap());
    }
}
