use super::{
    super::{Rational, ServiceError, VideoOptions},
    probe::Stream,
};

pub(super) const RESOLUTIONS: [(u16, u32, u32); 12] = [
    (144, 256, 144),
    (240, 426, 240),
    (360, 640, 360),
    (432, 768, 432),
    (480, 854, 480),
    (540, 960, 540),
    (576, 1024, 576),
    (720, 1280, 720),
    (900, 1600, 900),
    (1080, 1920, 1080),
    (1440, 2560, 1440),
    (2160, 3840, 2160),
];
pub(super) const FRAME_RATES: [u8; 10] = [10, 12, 15, 20, 24, 25, 30, 48, 50, 60];

#[derive(Clone)]
pub(super) struct Rendition {
    pub resolution: u16,
    pub width:      u32,
    pub height:     u32,
    pub fps:        u8,
    pub rate:       Rational,
    pub maxrate:    u64,
    pub level:      Level,
    pub references: u32,
}
impl Rendition {
    pub fn id(&self) -> String {
        format!("{}p{}", self.resolution, self.fps)
    }

    pub fn frame_duration(&self) -> f64 {
        f64::from(self.rate.denominator) / f64::from(self.rate.numerator)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Level {
    pub code:    u8,
    pub frames:  u32,
    pub mbps:    u32,
    pub dpb:     u32,
    pub bitrate: u64,
    pub buffer:  u64,
}
pub(super) const LEVELS: [Level; 16] = [
    Level {
        code:    10,
        frames:  99,
        mbps:    1485,
        dpb:     396,
        bitrate: 64_000,
        buffer:  175_000,
    },
    Level {
        code:    11,
        frames:  396,
        mbps:    3000,
        dpb:     900,
        bitrate: 192_000,
        buffer:  500_000,
    },
    Level {
        code:    12,
        frames:  396,
        mbps:    6000,
        dpb:     2376,
        bitrate: 384_000,
        buffer:  1_000_000,
    },
    Level {
        code:    13,
        frames:  396,
        mbps:    11880,
        dpb:     2376,
        bitrate: 768_000,
        buffer:  2_000_000,
    },
    Level {
        code:    20,
        frames:  396,
        mbps:    11880,
        dpb:     2376,
        bitrate: 2_000_000,
        buffer:  2_000_000,
    },
    Level {
        code:    21,
        frames:  792,
        mbps:    19800,
        dpb:     4752,
        bitrate: 4_000_000,
        buffer:  4_000_000,
    },
    Level {
        code:    22,
        frames:  1620,
        mbps:    20250,
        dpb:     8100,
        bitrate: 4_000_000,
        buffer:  4_000_000,
    },
    Level {
        code:    30,
        frames:  1620,
        mbps:    40500,
        dpb:     8100,
        bitrate: 10_000_000,
        buffer:  10_000_000,
    },
    Level {
        code:    31,
        frames:  3600,
        mbps:    108000,
        dpb:     18000,
        bitrate: 14_000_000,
        buffer:  14_000_000,
    },
    Level {
        code:    32,
        frames:  5120,
        mbps:    216000,
        dpb:     20480,
        bitrate: 20_000_000,
        buffer:  20_000_000,
    },
    Level {
        code:    40,
        frames:  8192,
        mbps:    245760,
        dpb:     32768,
        bitrate: 20_000_000,
        buffer:  25_000_000,
    },
    Level {
        code:    41,
        frames:  8192,
        mbps:    245760,
        dpb:     32768,
        bitrate: 50_000_000,
        buffer:  62_500_000,
    },
    Level {
        code:    42,
        frames:  8704,
        mbps:    522240,
        dpb:     34816,
        bitrate: 50_000_000,
        buffer:  62_500_000,
    },
    Level {
        code:    50,
        frames:  22080,
        mbps:    589824,
        dpb:     110400,
        bitrate: 135_000_000,
        buffer:  135_000_000,
    },
    Level {
        code:    51,
        frames:  36864,
        mbps:    983040,
        dpb:     184320,
        bitrate: 240_000_000,
        buffer:  240_000_000,
    },
    Level {
        code:    52,
        frames:  36864,
        mbps:    2073600,
        dpb:     184320,
        bitrate: 240_000_000,
        buffer:  240_000_000,
    },
];

pub(super) fn dimensions(stream: &Stream) -> (u32, u32) {
    let ratio = super::probe::rational(stream.text("sample_aspect_ratio"))
        .map_or(1.0, |ratio| f64::from(ratio.numerator) / f64::from(ratio.denominator));
    let width = (stream.number("width") * ratio).round().max(1.0) as u32;
    let height = stream.number("height") as u32;
    if stream.rotation().rem_euclid(180) == 90 { (height, width) } else { (width, height) }
}

pub(super) fn renditions(
    options: &VideoOptions,
    stream: &Stream,
    bitrate: u64,
) -> Result<Vec<Rendition>, ServiceError> {
    let (source_width, source_height) = dimensions(stream);
    if source_width == 0 || source_height == 0 {
        return Err(ServiceError::Invalid("invalid source video dimensions".into()));
    }
    let source_rate = stream
        .rate()
        .ok_or_else(|| ServiceError::Invalid("the source has no usable frame rate".into()))?;
    let rate = f64::from(source_rate.numerator) / f64::from(source_rate.denominator);
    let source_fps = FRAME_RATES
        .iter()
        .copied()
        .rfind(|fps| {
            rate + 0.0001 >= f64::from(*fps)
                || [24, 30, 60].contains(fps)
                    && (rate - f64::from(*fps) * 1000.0 / 1001.0).abs() < 0.0001
        })
        .unwrap_or(10);
    let maximum_resolution = source_width.min(source_height);
    let mut outputs = Vec::<Rendition>::new();
    for requested in &options.variants {
        let (resolution, width, height) = RESOLUTIONS
            .iter()
            .copied()
            .rfind(|(tier, ..)| {
                *tier <= requested.resolution && u32::from(*tier) <= maximum_resolution
            })
            .unwrap_or(RESOLUTIONS[0]);
        let fps = requested.fps.min(source_fps);
        if outputs.iter().any(|output| output.resolution == resolution && output.fps == fps) {
            continue;
        }
        let exact = if fps == source_fps && (rate - f64::from(fps) * 1000.0 / 1001.0).abs() < 0.0001
        {
            Rational {
                numerator: u32::from(fps) * 1000, denominator: 1001
            }
        } else {
            Rational {
                numerator: u32::from(fps), denominator: 1
            }
        };
        let maxrate = if resolution == 1080 && fps == 60 {
            bitrate
        } else {
            let limit = bitrate as f64
                * (f64::from(width) * f64::from(height) / 2_073_600.0).powf(0.75)
                * (f64::from(fps) / 60.0).sqrt();
            ((limit / 50_000.0).ceil() as u64).checked_mul(50_000).ok_or_else(|| {
                ServiceError::Unsupported("video bitrate exceeds its limit".into())
            })?
        };
        let macroblocks = width.div_ceil(16) * height.div_ceil(16);
        let level = LEVELS
            .iter()
            .copied()
            .find(|level| {
                macroblocks <= level.frames
                    && f64::from(macroblocks) * f64::from(exact.numerator)
                        / f64::from(exact.denominator)
                        <= f64::from(level.mbps)
                    && maxrate <= level.bitrate * 5 / 4
                    && maxrate.saturating_mul(2) <= level.buffer * 5 / 4
            })
            .ok_or_else(|| {
                ServiceError::Unsupported("video bitrate exceeds supported H.264 levels".into())
            })?;
        outputs.push(Rendition {
            resolution,
            width,
            height,
            fps,
            rate: exact,
            maxrate,
            level,
            references: (level.dpb / macroblocks).clamp(1, 16),
        });
    }
    outputs.sort_by_key(|output| (output.resolution, output.fps));
    Ok(outputs)
}

pub(super) fn can_copy(stream: &Stream, rendition: &Rendition) -> bool {
    let Some(info) = stream.h264() else {
        return false;
    };
    let Some(level) = LEVELS.iter().find(|level| level.code == info.level) else {
        return false;
    };
    let Some(rate) = stream.rate() else {
        return false;
    };
    let max_buffer = info
        .buffering
        .unwrap_or(info.references.saturating_add(stream.number("has_b_frames") as u32));
    stream.text("codec_name") == "h264"
        && stream.text("pix_fmt") == "yuv420p"
        && !stream.hdr()
        && matches!(stream.text("profile"), "High" | "Main" | "Baseline" | "Constrained Baseline")
        && stream.number("width") as u32 == rendition.width
        && stream.number("height") as u32 == rendition.height
        && stream.rotation() == 0
        && matches!(stream.text("sample_aspect_ratio"), "" | "N/A" | "1:1")
        && (f64::from(rate.numerator) / f64::from(rate.denominator)
            - 1.0 / rendition.frame_duration())
        .abs()
            < 0.000001
        && info.progressive
        && info.level <= rendition.level.code
        && info.macroblocks <= level.frames
        && f64::from(info.macroblocks) / rendition.frame_duration() <= f64::from(level.mbps)
        && info.references <= 16
        && max_buffer <= 16
        && max_buffer.max(info.references).saturating_mul(info.macroblocks) <= level.dpb
}

pub(super) fn filters(stream: &Stream, rendition: &Rendition, origin: f64) -> String {
    let (width, height) = dimensions(stream);
    let scale = (f64::from(rendition.width) / f64::from(width))
        .min(f64::from(rendition.height) / f64::from(height))
        .min(1.0);
    let width = ((f64::from(width) * scale).floor() as u32 / 2 * 2).max(2);
    let height = ((f64::from(height) * scale).floor() as u32 / 2 * 2).max(2);
    let mut filters = vec![format!("setpts=PTS-({origin:.9})/TB")];
    if matches!(stream.text("field_order"), "tt" | "bb" | "tb" | "bt") {
        filters.push("bwdif=mode=send_frame:deint=all".into());
    }
    if stream.hdr() {
        filters.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:\
             desat=0,zscale=t=bt709:m=bt709:r=tv"
                .into(),
        );
    }
    filters.push(format!(
        "scale={width}:{height}:flags=lanczos,setsar=1,pad={}:{}:(ow-iw)/2:(oh-ih)/2:color=black,\
         format=yuv420p,fps={}/{}:start_time=0:eof_action=pass",
        rendition.width, rendition.height, rendition.rate.numerator, rendition.rate.denominator
    ));
    filters.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VideoVariantSpec;

    fn source(width: u32, height: u32, rate: &str) -> Stream {
        Stream(
            serde_json::json!({"width": width, "height": height, "avg_frame_rate": rate, "sample_aspect_ratio": "1:1"}),
        )
    }

    #[test]
    fn tiers_follow_source_size_and_cadence_with_only_the_ten_fps_exception() {
        let options = VideoOptions {
            variants: vec![VideoVariantSpec {
                resolution: 1080, fps: 60
            }],
            ..VideoOptions::default()
        };
        let output = renditions(&options, &source(1760, 990, "26/1"), 12_000_000).unwrap();
        assert_eq!(
            (900, 25, 1600, 900),
            (output[0].resolution, output[0].fps, output[0].width, output[0].height)
        );
        let square = renditions(&options, &source(722, 722, "60/1"), 12_000_000).unwrap();
        assert_eq!((720, 1280, 720), (square[0].resolution, square[0].width, square[0].height));
        let small_source = source(64, 64, "2/1");
        let small = renditions(&options, &small_source, 12_000_000).unwrap();
        assert_eq!(
            (144, 10, 256, 144),
            (small[0].resolution, small[0].fps, small[0].width, small[0].height)
        );
        assert!(filters(&small_source, &small[0], 0.0).contains("scale=64:64"));
    }

    #[test]
    fn baseline_rate_stays_exact_and_level_limits_the_reference_buffer() {
        let options = VideoOptions {
            variants: vec![VideoVariantSpec {
                resolution: 1080, fps: 60
            }],
            ..VideoOptions::default()
        };
        let output = renditions(&options, &source(1920, 1080, "60/1"), 12_000_001).unwrap();
        assert_eq!(12_000_001, output[0].maxrate);
        assert_eq!(42, output[0].level.code);
        assert_eq!(4, output[0].references);
    }
}
