use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use super::super::ServiceError;

pub(super) struct Timing {
    pub start: i64,
    pub end:   i64,
}

// Move the generated single-track edit offset into fragment decode times.
// HLS readers can then share one clock without relying on movie edit lists.
pub(super) fn normalize_timeline(directory: &Path) -> Result<(), ServiceError> {
    let mut init =
        std::fs::OpenOptions::new().read(true).write(true).open(directory.join("init.mp4"))?;
    let size = init.metadata()?.len();
    let mut boxes = Vec::new();
    walk(&mut init, 0, size, &mut boxes)?;
    let Some((_, _, edit)) = boxes.iter().find(|(name, ..)| name == b"elst") else {
        return Ok(());
    };
    let movie = &boxes
        .iter()
        .find(|(name, ..)| name == b"mvhd")
        .ok_or_else(|| invalid("missing movie clock"))?
        .2;
    let media = &boxes
        .iter()
        .find(|(name, ..)| name == b"mdhd")
        .ok_or_else(|| invalid("missing media clock"))?
        .2;
    let movie_clock = u32_at(movie, if movie.first() == Some(&1) { 20 } else { 12 })?;
    let media_clock = u32_at(media, if media.first() == Some(&1) { 20 } else { 12 })?;
    if movie_clock == 0 || media_clock == 0 {
        return Err(invalid("invalid MP4 clock"));
    }
    let count = u32_at(edit, 4)?;
    if count == 0 || count > 2 {
        return Err(invalid("unexpected HLS movie edit list"));
    }
    let extended = edit.first() == Some(&1);
    let mut empty = 0u64;
    let mut media_start = None;
    for index in 0..count as usize {
        let offset = 8 + index * if extended { 20 } else { 12 };
        let duration =
            if extended { u64_at(edit, offset)? } else { u64::from(u32_at(edit, offset)?) };
        let time = if extended {
            u64_at(edit, offset + 8)? as i64
        } else {
            i64::from(u32_at(edit, offset + 4)? as i32)
        };
        let rate = u32_at(edit, offset + if extended { 16 } else { 8 })?;
        if rate != 0x0001_0000 {
            return Err(invalid("unsupported HLS movie edit rate"));
        }
        if time == -1 && media_start.is_none() {
            empty = empty
                .checked_add(duration)
                .ok_or_else(|| invalid("movie time exceeds its limit"))?;
        } else if time >= 0 && media_start.is_none() {
            media_start = Some(time);
        } else {
            return Err(invalid("unexpected HLS movie edit sequence"));
        }
    }
    let media_start = media_start.ok_or_else(|| invalid("missing HLS media edit"))?;
    let delta = (i128::from(empty) * i128::from(media_clock) + i128::from(movie_clock / 2))
        / i128::from(movie_clock)
        - i128::from(media_start);
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("segment-") || !name.ends_with(".m4s") {
            continue;
        }
        let mut file = std::fs::OpenOptions::new().read(true).write(true).open(entry.path())?;
        let size = file.metadata()?.len();
        let mut boxes = Vec::new();
        walk(&mut file, 0, size, &mut boxes)?;
        for (_, offset, data) in boxes.iter().filter(|(name, ..)| name == b"tfdt") {
            let extended = data.first() == Some(&1);
            let base = if extended { u64_at(data, 4)? } else { u64::from(u32_at(data, 4)?) };
            let shifted = u64::try_from(i128::from(base) + delta)
                .map_err(|_| invalid("HLS decode time is outside its clock"))?;
            file.seek(SeekFrom::Start(offset + 4))?;
            if extended {
                file.write_all(&shifted.to_be_bytes())?;
            } else {
                file.write_all(
                    &u32::try_from(shifted)
                        .map_err(|_| invalid("HLS decode time exceeds the box width"))?
                        .to_be_bytes(),
                )?;
            }
        }
        for (_, offset, data) in boxes.iter().filter(|(name, ..)| name == b"sidx") {
            let clock = u32_at(data, 8)?;
            if clock == 0 {
                return Err(invalid("invalid segment index clock"));
            }
            let shift =
                (delta * i128::from(clock) + i128::from(media_clock / 2)) / i128::from(media_clock);
            let extended = data.first() == Some(&1);
            let base = if extended { u64_at(data, 12)? } else { u64::from(u32_at(data, 12)?) };
            let shifted = u64::try_from(i128::from(base) + shift)
                .map_err(|_| invalid("HLS index time exceeds its clock"))?;
            file.seek(SeekFrom::Start(offset + 12))?;
            if extended {
                file.write_all(&shifted.to_be_bytes())?;
            } else {
                file.write_all(
                    &u32::try_from(shifted)
                        .map_err(|_| invalid("HLS index time exceeds the box width"))?
                        .to_be_bytes(),
                )?;
            }
        }
    }
    let edit_offset = boxes
        .iter()
        .find(|(name, ..)| name == b"edts")
        .ok_or_else(|| invalid("missing movie edit container"))?
        .1;
    init.seek(SeekFrom::Start(edit_offset - 4))?;
    init.write_all(b"free")?;
    Ok(())
}

pub(super) fn fragment_timing(path: &Path) -> Result<Timing, ServiceError> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut boxes = Vec::new();
    walk(&mut file, 0, size, &mut boxes)?;
    let tfdt = boxes
        .iter()
        .find(|(name, ..)| name == b"tfdt")
        .ok_or_else(|| invalid("missing fragment decode time"))?
        .2
        .as_slice();
    let base = if tfdt.first() == Some(&1) {
        u64::from_be_bytes(
            tfdt.get(4..12).ok_or_else(|| invalid("invalid tfdt"))?.try_into().unwrap(),
        )
    } else {
        u64::from(u32_at(tfdt, 4)?)
    };
    let mut default_duration = 0;
    if let Some((_, _, data)) = boxes.iter().find(|(name, ..)| name == b"tfhd") {
        let flags = u32_at(data, 0)? & 0x00FF_FFFF;
        let offset = 8 + if flags & 1 != 0 { 8 } else { 0 } + if flags & 2 != 0 { 4 } else { 0 };
        if flags & 8 != 0 {
            default_duration = u32_at(data, offset)?;
        }
    }
    let mut dts = i64::try_from(base).map_err(|_| invalid("fragment time exceeds its limit"))?;
    let mut start = i64::MAX;
    let mut end = i64::MIN;
    for (_, _, data) in boxes.iter().filter(|(name, ..)| name == b"trun") {
        let flags = u32_at(data, 0)? & 0x00FF_FFFF;
        let count = u32_at(data, 4)?;
        if count > 100_000 {
            return Err(invalid("too many samples in a fragment"));
        }
        let mut offset =
            8 + if flags & 1 != 0 { 4 } else { 0 } + if flags & 4 != 0 { 4 } else { 0 };
        for _ in 0..count {
            let duration = if flags & 0x100 != 0 {
                let value = u32_at(data, offset)?;
                offset += 4;
                value
            } else {
                default_duration
            };
            if flags & 0x200 != 0 {
                u32_at(data, offset)?;
                offset += 4;
            }
            if flags & 0x400 != 0 {
                u32_at(data, offset)?;
                offset += 4;
            }
            let composition = if flags & 0x800 != 0 {
                let value = u32_at(data, offset)?;
                offset += 4;
                if data.first() == Some(&1) { i64::from(value as i32) } else { i64::from(value) }
            } else {
                0
            };
            if duration == 0 {
                return Err(invalid("fragment has no sample duration"));
            }
            let pts = dts
                .checked_add(composition)
                .ok_or_else(|| invalid("fragment time exceeds its limit"))?;
            start = start.min(pts);
            end = end.max(
                pts.checked_add(i64::from(duration))
                    .ok_or_else(|| invalid("fragment time exceeds its limit"))?,
            );
            dts = dts
                .checked_add(i64::from(duration))
                .ok_or_else(|| invalid("fragment time exceeds its limit"))?;
        }
    }
    if end <= start {
        return Err(invalid("fragment has no presentation samples"));
    }
    Ok(Timing {
        start,
        end,
    })
}

fn walk(
    file: &mut File,
    start: u64,
    end: u64,
    output: &mut Vec<([u8; 4], u64, Vec<u8>)>,
) -> Result<(), ServiceError> {
    let mut offset = start;
    while offset < end {
        if end - offset < 8 {
            return Err(invalid("incomplete MP4 box"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0; 8];
        file.read_exact(&mut header)?;
        let name = header[4..].try_into().unwrap();
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
        let header_size = if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended)?;
            size = u64::from_be_bytes(extended);
            16
        } else {
            8
        };
        if size == 0 {
            size = end - offset;
        }
        if size < header_size || size > end - offset {
            return Err(invalid("invalid MP4 box size"));
        }
        if matches!(&name, b"moof" | b"traf" | b"moov" | b"trak" | b"mdia" | b"edts") {
            if &name == b"edts" {
                output.push((name, offset + header_size, Vec::new()));
            }
            walk(file, offset + header_size, offset + size, output)?;
        } else if matches!(
            &name,
            b"tfdt" | b"tfhd" | b"trun" | b"elst" | b"mvhd" | b"mdhd" | b"sidx"
        ) {
            if size - header_size > 1024 * 1024 {
                return Err(invalid("MP4 timing metadata exceeds its limit"));
            }
            let mut data = vec![0; (size - header_size) as usize];
            file.read_exact(&mut data)?;
            output.push((name, offset + header_size, data));
        }
        offset += size;
    }
    Ok(())
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, ServiceError> {
    bytes
        .get(offset..offset + 4)
        .map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap()))
        .ok_or_else(|| invalid("incomplete MP4 timing metadata"))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, ServiceError> {
    bytes
        .get(offset..offset + 8)
        .map(|bytes| u64::from_be_bytes(bytes.try_into().unwrap()))
        .ok_or_else(|| invalid("incomplete MP4 timing metadata"))
}
fn invalid(message: &str) -> ServiceError {
    ServiceError::Invalid(message.into())
}
