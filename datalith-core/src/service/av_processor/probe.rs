use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::AtomicBool,
};

use serde_json::Value;

use super::{
    super::{DatalithService, Rational, ServiceError},
    executor::{input_options, push},
};

#[derive(Clone)]
pub(super) struct Stream(pub Value);

impl Stream {
    pub fn text(&self, key: &str) -> &str {
        self.0[key].as_str().unwrap_or("")
    }

    pub fn number(&self, key: &str) -> f64 {
        self.0[key]
            .as_f64()
            .or_else(|| self.0[key].as_str().and_then(|value| value.parse().ok()))
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    }

    pub fn index(&self) -> u32 {
        self.number("index") as u32
    }

    pub fn rate(&self) -> Option<Rational> {
        self.0["_nominal_cadence_verified"]
            .as_bool()
            .filter(|verified| *verified)
            .and_then(|_| self.h264().and_then(|info| info.frame_rate))
            .or_else(|| rational(self.text("avg_frame_rate")))
            .or_else(|| rational(self.text("r_frame_rate")))
    }

    pub fn hdr(&self) -> bool {
        matches!(self.text("color_transfer"), "smpte2084" | "arib-std-b67")
    }

    pub fn rotation(&self) -> i32 {
        self.0["side_data_list"]
            .as_array()
            .and_then(|list| list.iter().find_map(|item| item["rotation"].as_i64()))
            .unwrap_or(0) as i32
    }

    pub fn bit_depth(&self) -> u8 {
        let raw = self.number("bits_per_raw_sample") as u8;
        if raw != 0 {
            raw
        } else {
            let bits = self.number("bits_per_sample") as u8;
            if bits != 0 {
                bits
            } else if self.text("sample_fmt").starts_with("s16") {
                16
            } else if self.text("sample_fmt").starts_with("s32") {
                32
            } else {
                0
            }
        }
    }

    pub fn lossless(&self, executor: &super::executor::Executor) -> bool {
        executor.pure_lossless.contains(self.text("codec_name"))
    }

    pub fn flac_compatible(&self, executor: &super::executor::Executor) -> bool {
        self.lossless(executor)
            && matches!(self.text("sample_fmt"), "u8" | "s16" | "s16p" | "s32" | "s32p")
            && (1..=32).contains(&self.bit_depth())
    }

    pub fn h264(&self) -> Option<H264Info> {
        let bytes = decode_hex_dump(self.text("extradata"))?;
        if bytes.len() < 8 || bytes[0] != 1 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[6], bytes[7]]));
        let sps = bytes.get(8..8usize.checked_add(length)?)?;
        let mut rbsp = Vec::new();
        let mut zeros = 0;
        for &byte in sps.get(1..)? {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            rbsp.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        let mut bits = Bits {
            bytes: &rbsp, offset: 0
        };
        let profile = bits.read(8)?;
        let compatibility = bits.read(8)?;
        let level = bits.read(8)?;
        if profile != u32::from(bytes[1])
            || compatibility != u32::from(bytes[2])
            || level != u32::from(bytes[3])
        {
            return None;
        }
        // Level 1b uses level 11 with constraint_set3, so do not treat it as level 1.1.
        if matches!(profile, 66 | 77 | 88) && level == 11 && compatibility & 0x10 != 0 {
            return None;
        }
        bits.ue()?;
        if matches!(
            profile,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            let chroma = bits.ue()?;
            if chroma == 3 {
                bits.read(1)?;
            }
            bits.ue()?;
            bits.ue()?;
            bits.read(1)?;
            if bits.flag()? {
                for index in 0..if chroma == 3 { 12 } else { 8 } {
                    if bits.flag()? {
                        let mut last = 8i32;
                        let mut next = 8i32;
                        for _ in 0..if index < 6 { 16 } else { 64 } {
                            if next != 0 {
                                next = (last + bits.se()? + 256) % 256;
                            }
                            if next != 0 {
                                last = next;
                            }
                        }
                    }
                }
            }
        }
        bits.ue()?;
        match bits.ue()? {
            0 => {
                bits.ue()?;
            },
            1 => {
                bits.read(1)?;
                bits.se()?;
                bits.se()?;
                let count = bits.ue()?;
                if count > 256 {
                    return None;
                }
                for _ in 0..count {
                    bits.se()?;
                }
            },
            2 => (),
            _ => return None,
        }
        let references = bits.ue()?;
        bits.read(1)?;
        let width_mbs = bits.ue()?.checked_add(1)?;
        let height_mbs = bits.ue()?.checked_add(1)?;
        let progressive = bits.flag()?;
        if !progressive {
            bits.read(1)?;
        }
        bits.read(1)?;
        if bits.flag()? {
            for _ in 0..4 {
                bits.ue()?;
            }
        }
        let mut buffering = None;
        let mut frame_rate = None;
        if bits.flag()? {
            if bits.flag()? && bits.read(8)? == 255 {
                bits.read(32)?;
            }
            if bits.flag()? {
                bits.read(1)?;
            }
            if bits.flag()? {
                bits.read(4)?;
                if bits.flag()? {
                    bits.read(24)?;
                }
            }
            if bits.flag()? {
                bits.ue()?;
                bits.ue()?;
            }
            if bits.flag()? {
                let units = bits.read(32)?;
                let scale = bits.read(32)?;
                bits.read(1)?;
                if units > 0 && scale > 0 {
                    frame_rate = units.checked_mul(2).map(|denominator| Rational {
                        numerator: scale,
                        denominator,
                    });
                }
            }
            let nal_hrd = bits.flag()?;
            if nal_hrd {
                bits.hrd()?;
            }
            let vcl_hrd = bits.flag()?;
            if vcl_hrd {
                bits.hrd()?;
            }
            if nal_hrd || vcl_hrd {
                bits.read(1)?;
            }
            bits.read(1)?;
            if bits.flag()? {
                bits.read(1)?;
                for _ in 0..5 {
                    bits.ue()?;
                }
                buffering = Some(bits.ue()?);
            }
        }
        Some(H264Info {
            codec: format!("avc1.{:02x}{:02x}{:02x}", bytes[1], bytes[2], bytes[3]),
            profile: bytes[1],
            level: bytes[3],
            references,
            buffering,
            progressive,
            macroblocks: width_mbs.checked_mul(height_mbs)?.checked_mul(if progressive {
                1
            } else {
                2
            })?,
            nal_length: usize::from((bytes[4] & 3) + 1),
            frame_rate,
        })
    }
}

pub(super) struct H264Info {
    pub frame_rate:  Option<Rational>,
    pub codec:       String,
    pub profile:     u8,
    pub level:       u8,
    pub references:  u32,
    pub buffering:   Option<u32>,
    pub progressive: bool,
    pub macroblocks: u32,
    pub nal_length:  usize,
}

struct Bits<'a> {
    bytes:  &'a [u8],
    offset: usize,
}
impl Bits<'_> {
    fn read(&mut self, count: usize) -> Option<u32> {
        if count > 32 || self.offset.checked_add(count)? > self.bytes.len().checked_mul(8)? {
            return None;
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | u32::from((self.bytes[self.offset / 8] >> (7 - self.offset % 8)) & 1);
            self.offset += 1;
        }
        Some(value)
    }

    fn flag(&mut self) -> Option<bool> {
        Some(self.read(1)? != 0)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            if zeros >= 32 {
                return None;
            }
        }
        (1u32 << zeros).checked_sub(1)?.checked_add(self.read(zeros)?)
    }

    fn se(&mut self) -> Option<i32> {
        let code = self.ue()?;
        Some(if code % 2 == 0 {
            -(i32::try_from(code / 2).ok()?)
        } else {
            i32::try_from(code / 2 + 1).ok()?
        })
    }

    fn hrd(&mut self) -> Option<()> {
        let count = self.ue()?.checked_add(1)?;
        if count > 32 {
            return None;
        }
        self.read(8)?;
        for _ in 0..count {
            self.ue()?;
            self.ue()?;
            self.read(1)?;
        }
        self.read(20)?;
        Some(())
    }
}

fn decode_hex_dump(value: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    for line in value.lines().filter(|line| line.contains(':')) {
        let data = line.split_once(':')?.1.trim_start().split("  ").next()?;
        for word in data.split_whitespace() {
            if word.len() % 2 != 0 {
                return None;
            }
            bytes.extend(hex::decode(word).ok()?);
        }
    }
    if bytes.is_empty() { None } else { Some(bytes) }
}

pub(super) fn rational(value: &str) -> Option<Rational> {
    let (numerator, denominator) = value.split_once('/').or_else(|| value.split_once(':'))?;
    let rate = Rational {
        numerator:   numerator.parse().ok()?,
        denominator: denominator.parse().ok()?,
    };
    if rate.numerator == 0 || rate.denominator == 0 { None } else { Some(rate) }
}

pub(super) struct Probe {
    pub streams: Vec<Stream>,
    pub format:  Value,
}
impl Probe {
    pub fn audio(&self, requested: Option<u32>) -> Result<Option<&Stream>, ServiceError> {
        if let Some(index) = requested {
            return self
                .streams
                .iter()
                .find(|stream| stream.index() == index && stream.text("codec_type") == "audio")
                .map(Some)
                .ok_or_else(|| {
                    ServiceError::Invalid("the selected audio stream does not exist".into())
                });
        }
        Ok(self
            .streams
            .iter()
            .find(|stream| {
                stream.text("codec_type") == "audio"
                    && stream.0["disposition"]["default"].as_i64() == Some(1)
            })
            .or_else(|| self.streams.iter().find(|stream| stream.text("codec_type") == "audio")))
    }
}

pub(super) async fn inspect(
    service: &DatalithService,
    input: &Path,
    cancel: &AtomicBool,
) -> Result<Probe, ServiceError> {
    let mut args: Vec<OsString> = ["-v", "error"].into_iter().map(Into::into).collect();
    args.extend(input_options());
    push(&mut args, &[
        "-show_entries",
        "format=format_name,duration,start_time:stream=index,codec_name,codec_type,profile,width,\
         height,pix_fmt,color_transfer,color_primaries,color_space,level,field_order,\
         sample_aspect_ratio,avg_frame_rate,r_frame_rate,time_base,start_time,duration,sample_fmt,\
         sample_rate,channels,bits_per_sample,bits_per_raw_sample,bit_rate,has_b_frames,extradata:\
         stream_disposition=default,attached_pic:stream_side_data=rotation",
        "-show_data",
        "-of",
        "json",
    ]);
    args.push(input.as_os_str().into());
    let output = service
        .0
        .av
        .run(&service.0.config.av.ffprobe, &args, cancel, None, true, |_| Ok(()))
        .await?;
    let value: Value = serde_json::from_str(&output)?;
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| ServiceError::Invalid("the source has no media streams".into()))?
        .iter()
        .cloned()
        .map(Stream)
        .collect();
    Ok(Probe {
        streams,
        format: value["format"].clone(),
    })
}

#[derive(Clone, Copy)]
pub(super) struct BitrateLimit {
    pub bitrate: u64,
    pub buffer:  u64,
}

#[derive(Clone, Copy)]
pub(super) struct VideoLimit {
    pub limits: [BitrateLimit; 2],
    pub frame:  f64,
    pub origin: f64,
}

#[derive(Clone, Default)]
pub(super) struct PacketStats {
    pub cadence:         bool,
    pub payload_bytes:   f64,
    pub bytes:           u64,
    pub count:           u64,
    pub first_pts:       Option<f64>,
    pub first_dts:       Option<f64>,
    pub end:             f64,
    pub skip_samples:    u32,
    pub discard_padding: u32,
    pub regular:         bool,
    pub envelope:        bool,
    pub aligned_keys:    bool,
    next_key:            f64,
    previous_dts:        Option<f64>,
    previous_duration:   Option<f64>,
    cadence_anchor:      Option<f64>,
    bucket_min:          [f64; 2],
}
impl PacketStats {
    pub fn duration(&self, rate: u32) -> f64 {
        (self.end
            - self.first_pts.unwrap_or(0.0)
            - (f64::from(self.skip_samples) + f64::from(self.discard_padding))
                / f64::from(rate.max(1)))
        .max(0.0)
    }

    pub fn start(&self, rate: u32) -> f64 {
        self.first_pts.unwrap_or(0.0) + f64::from(self.skip_samples) / f64::from(rate.max(1))
    }

    pub fn bitrate(&self, rate: u32) -> u64 {
        (self.bytes as f64 * 8.0 / self.duration(rate).max(0.001)).ceil() as u64
    }

    fn check_envelopes(&mut self, before: u64, dts: f64, limits: [BitrateLimit; 2]) {
        for (index, limit) in limits.iter().enumerate() {
            let previous = before as f64 * 8.0 - limit.bitrate as f64 * dts;
            if self.count == 1 {
                self.bucket_min[index] = previous;
            } else {
                self.bucket_min[index] = self.bucket_min[index].min(previous);
            }
            self.envelope &=
                self.bytes as f64 * 8.0 - limit.bitrate as f64 * dts - self.bucket_min[index]
                    <= limit.buffer as f64;
        }
    }
}

pub(super) async fn packets(
    service: &DatalithService,
    input: &Path,
    stream: &Stream,
    cancel: &AtomicBool,
    video_limit: Option<VideoLimit>,
) -> Result<PacketStats, ServiceError> {
    let mut args: Vec<OsString> = ["-v", "error"].into_iter().map(Into::into).collect();
    args.extend(input_options());
    push(&mut args, &[
        "-select_streams",
        &stream.index().to_string(),
        "-show_packets",
        "-show_entries",
        "packet=pts_time,dts_time,duration_time,size,pos,flags:packet_side_data=skip_samples,\
         discard_padding",
        "-of",
        "compact=p=0:nk=0",
    ]);
    args.push(input.as_os_str().into());
    let mut stats = PacketStats {
        regular: true,
        envelope: true,
        aligned_keys: true,
        ..PacketStats::default()
    };
    let info = stream.h264();
    stats.cadence = info.as_ref().and_then(|info| info.frame_rate).is_some();
    let mut source = File::open(input)?;
    service
        .0
        .av
        .run(&service.0.config.av.ffprobe, &args, cancel, None, false, |line| {
            let fields = line
                .split('|')
                .filter_map(|field| field.split_once('='))
                .map(|(key, value)| (key.rsplit(':').next().unwrap_or(key), value))
                .collect::<std::collections::HashMap<_, _>>();
            let number = |key| {
                fields
                    .get(key)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite())
            };
            let Some(size) = number("size") else {
                return Ok(());
            };
            let pts = number("pts_time");
            let dts = number("dts_time");
            let duration = number("duration_time").unwrap_or(0.0);
            let before = stats.bytes;
            stats.bytes =
                stats.bytes.checked_add(size as u64).ok_or(ServiceError::PayloadTooLarge)?;
            stats.count += 1;
            if let Some(rate) = info.as_ref().and_then(|info| info.frame_rate) {
                let frame = f64::from(rate.denominator) / f64::from(rate.numerator);
                let tolerance = 0.00002f64.max(frame / 1000.0);
                if stats.count == 2 {
                    stats.cadence_anchor = dts;
                }
                // A first-frame hold does not change the following source cadence.
                if stats.count > 2 {
                    stats.cadence &=
                        stats.previous_dts.zip(dts).is_some_and(|(previous, current)| {
                            (current - previous - frame).abs() <= tolerance
                        }) && stats
                            .previous_duration
                            .is_some_and(|duration| (duration - frame).abs() <= tolerance)
                            && stats.cadence_anchor.zip(dts).is_some_and(|(anchor, current)| {
                                (current - anchor - (stats.count - 2) as f64 * frame).abs()
                                    <= tolerance
                            });
                }
            }
            if let Some(pts) = pts {
                stats.first_pts = Some(stats.first_pts.map_or(pts, |first| first.min(pts)));
                stats.end = stats.end.max(pts + duration);
            } else {
                stats.regular = false;
            }
            if stats.count == 1 {
                stats.first_dts = dts;
                stats.skip_samples = number("skip_samples").unwrap_or(0.0) as u32;
            }
            if let Some(pts) = pts {
                let start = if stream.number("duration") > 0.0 {
                    stream.number("start_time")
                } else {
                    stats.start(stream.number("sample_rate") as u32)
                };
                let end = if stream.number("duration") > 0.0 {
                    start + stream.number("duration")
                } else {
                    f64::INFINITY
                };
                if duration > 0.0 {
                    stats.payload_bytes +=
                        size * ((pts + duration).min(end) - pts.max(start)).max(0.0) / duration;
                } else {
                    stats.payload_bytes += size;
                }
            }
            stats.discard_padding = number("discard_padding").unwrap_or(0.0) as u32;
            if let Some(VideoLimit {
                limits,
                frame,
                origin,
            }) = video_limit
            {
                let tolerance = 0.00002f64.max(frame / 1000.0);
                stats.regular &= (duration - frame).abs() <= tolerance
                    && dts.is_some()
                    && pts.is_some()
                    && stats.previous_dts.zip(dts).is_none_or(|(previous, current)| {
                        current > previous && (current - previous - frame).abs() <= tolerance
                    });
                if let Some(dts) = dts {
                    stats.check_envelopes(before, dts, limits);
                }
                if let Some(pts) = pts {
                    let position = (pts - origin) / frame;
                    stats.regular &= (position - position.round()).abs() <= tolerance / frame;
                    let relative = pts - origin;
                    let target = (stats.next_key / frame - 0.000001).ceil() * frame;
                    if relative >= target - tolerance {
                        if (relative - target).abs() <= tolerance
                            && fields.get("flags").is_some_and(|flags| flags.contains('K'))
                        {
                            let idr =
                                info.as_ref().zip(number("pos")).is_some_and(|(info, pos)| {
                                    avcc_idr(&mut source, pos as u64, size as u64, info.nal_length)
                                        .unwrap_or(false)
                                });
                            stats.aligned_keys &= idr;
                            stats.next_key += 2.0;
                        } else if relative > target + tolerance {
                            stats.aligned_keys = false;
                        }
                    }
                }
            }
            stats.previous_dts = dts;
            stats.previous_duration = Some(duration);
            Ok(())
        })
        .await?;
    if stats.count == 0 || stats.first_pts.is_none() {
        return Err(ServiceError::Invalid("the selected stream has no timed packets".into()));
    }
    Ok(stats)
}

fn avcc_idr(
    file: &mut File,
    position: u64,
    size: u64,
    length_size: usize,
) -> std::io::Result<bool> {
    let mut offset = 0u64;
    while offset + (length_size as u64) < size {
        file.seek(SeekFrom::Start(position + offset))?;
        let mut length = [0u8; 4];
        file.read_exact(&mut length[4 - length_size..])?;
        let count = u64::from(u32::from_be_bytes(length));
        if count == 0 || offset + length_size as u64 + count > size {
            return Ok(false);
        }
        let mut header = [0];
        file.read_exact(&mut header)?;
        if header[0] & 31 == 5 {
            return Ok(true);
        }
        offset += length_size as u64 + count;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_buffer_and_recipe_rate_are_checked_independently() {
        let recipe = BitrateLimit {
            bitrate: 1000, buffer: 2000
        };
        let source = BitrateLimit {
            bitrate: 2000, buffer: 1000
        };
        let accepted = |limits, packets: &[(f64, u64)]| {
            let mut stats = PacketStats {
                envelope: true,
                ..PacketStats::default()
            };
            for &(dts, size) in packets {
                let before = stats.bytes;
                stats.bytes += size;
                stats.count += 1;
                stats.check_envelopes(before, dts, limits);
            }
            stats.envelope
        };
        let burst = [(0.0, 64), (0.1, 128)];
        assert!(accepted([recipe, recipe], &burst));
        assert!(!accepted([recipe, source], &burst));
        let sustained: Vec<_> = (0..6).map(|index| (f64::from(index) * 0.6, 120)).collect();
        assert!(accepted([source, source], &sustained));
        assert!(!accepted([recipe, source], &sustained));
        let valid = [(0.0, 64), (1.0, 120), (2.0, 64)];
        assert!(accepted([recipe, source], &valid));
    }
}
