use std::collections::{HashMap, VecDeque};

use sqlx::Row;
use uuid::Uuid;

use super::{
    Content, DatalithService, HlsAsset, HlsAudioFilter, HlsInventory, HlsPlaylist, HlsTrack,
    MediaFile, MediaKind, ServiceError,
};
use crate::guard::OpenGuard;

impl DatalithService {
    pub(super) async fn hls_inventory(&self, id: Uuid) -> Result<HlsInventory, ServiceError> {
        let value: String = sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
            .bind(id)
            .fetch_optional(&self.0.datalith.0.db)
            .await?
            .ok_or(ServiceError::NotFound)?;
        Ok(serde_json::from_str(&value)?)
    }

    /// Render the playable video and audio combinations for this media.
    pub async fn hls_master(
        &self,
        id: Uuid,
        audio: HlsAudioFilter,
        session: Option<&str>,
    ) -> Result<HlsPlaylist, ServiceError> {
        let media = self.authorize_media(id, session).await?;
        let video = media.video.as_ref().ok_or(ServiceError::NotFound)?;
        let inventory = self.hls_inventory(id).await?;
        let target = target_duration(&inventory)?;
        let tracks: HashMap<_, _> =
            inventory.tracks.iter().map(|track| (track.id.as_str(), track)).collect();
        let mut rates = HashMap::new();
        for track in &inventory.tracks {
            rates.insert(track.id.as_str(), bandwidth(track, target)?);
        }
        let suffix = if media.single_use {
            format!("?session={}", session.ok_or(ServiceError::NotFound)?)
        } else {
            String::new()
        };
        let mut body = String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n");
        let allowed = |codec: &str| match audio {
            HlsAudioFilter::Aac => codec == "aac",
            HlsAudioFilter::All => true,
            HlsAudioFilter::Flac => codec == "flac",
        };
        for output in video.audio.iter().filter(|output| allowed(&output.codec)) {
            body.push_str(&format!(
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"main\",DEFAULT=YES,AUTOSELECT=YES,\
                 CHANNELS=\"{}\",URI=\"{}/index.m3u8{suffix}\"\n",
                output.id, output.channels, output.id
            ));
        }
        let mut combinations = 0;
        for variant in &video.variants {
            let track = tracks
                .get(variant.id.as_str())
                .ok_or_else(|| ServiceError::Internal("video track inventory is missing".into()))?;
            let (peak, average) = rates[variant.id.as_str()];
            let fps =
                f64::from(variant.frame_rate.numerator) / f64::from(variant.frame_rate.denominator);
            let attributes =
                format!("RESOLUTION={}x{},FRAME-RATE={fps:.3}", variant.width, variant.height);
            if video.audio.is_empty() {
                body.push_str(&format!(
                    "#EXT-X-STREAM-INF:BANDWIDTH={peak},AVERAGE-BANDWIDTH={average},CODECS=\"{}\",\
                     {attributes}\n{}/index.m3u8{suffix}\n",
                    track.codec, variant.id
                ));
                combinations += 1;
            } else {
                for output in video
                    .audio
                    .iter()
                    .filter(|output| variant.audio.contains(&output.id) && allowed(&output.codec))
                {
                    let audio_track = tracks.get(output.id.as_str()).ok_or_else(|| {
                        ServiceError::Internal("audio track inventory is missing".into())
                    })?;
                    let (audio_peak, audio_average) = rates[output.id.as_str()];
                    body.push_str(&format!(
                        "#EXT-X-STREAM-INF:BANDWIDTH={},AVERAGE-BANDWIDTH={},CODECS=\"{},{}\",\
                         AUDIO=\"{}\",{attributes}\n{}/index.m3u8{suffix}\n",
                        peak.saturating_add(audio_peak),
                        average.saturating_add(audio_average),
                        track.codec,
                        audio_track.codec,
                        output.id,
                        variant.id
                    ));
                    combinations += 1;
                }
            }
        }
        if combinations == 0 {
            return Err(ServiceError::NotFound);
        }
        Ok(HlsPlaylist {
            body,
            temporary: media.single_use || media.expires_at.is_some(),
        })
    }

    /// Render one video or audio track with a shared target duration.
    pub async fn hls_track(
        &self,
        id: Uuid,
        track: &str,
        session: Option<&str>,
    ) -> Result<HlsPlaylist, ServiceError> {
        let media = self.authorize_media(id, session).await?;
        if media.kind != MediaKind::Video {
            return Err(ServiceError::NotFound);
        }
        let inventory = self.hls_inventory(id).await?;
        let target = target_duration(&inventory)?;
        let track = inventory
            .tracks
            .iter()
            .find(|candidate| candidate.id == track)
            .ok_or(ServiceError::NotFound)?;
        let suffix = if media.single_use {
            format!("?session={}", session.ok_or(ServiceError::NotFound)?)
        } else {
            String::new()
        };
        Ok(HlsPlaylist {
            body:      track_playlist(track, target, &suffix),
            temporary: media.single_use || media.expires_at.is_some(),
        })
    }

    /// Open a track asset without loading the full segment inventory.
    pub async fn open_hls_content(
        &self,
        id: Uuid,
        track: &str,
        asset: HlsAsset,
        session: Option<&str>,
    ) -> Result<Content, ServiceError> {
        let media = self.authorize_media(id, session).await?;
        let video = media.video.as_ref().ok_or(ServiceError::NotFound)?;
        let is_video = video.variants.iter().any(|variant| variant.id == track);
        if !is_video && !video.audio.iter().any(|audio| audio.id == track) {
            return Err(ServiceError::NotFound);
        }
        let (role, name) = match asset {
            HlsAsset::Initialization => (format!("hls:{track}:init"), format!("{track}-init.mp4")),
            HlsAsset::Segment(sequence) => {
                (format!("hls:{track}:{sequence}"), format!("{track}-segment-{sequence:06}.m4s"))
            },
        };
        let row = sqlx::query(
            "SELECT f.id, COALESCE(b.hash,f.hash) AS hash, f.file_size FROM media_files m JOIN \
             files f ON f.id=m.file_id LEFT JOIN blob_files b ON b.file_id=f.id WHERE \
             m.media_id=? AND m.role=?",
        )
        .bind(id)
        .bind(role)
        .fetch_optional(&self.0.datalith.0.db)
        .await?
        .ok_or(ServiceError::NotFound)?;
        let file_id: Uuid = row.try_get("id")?;
        let guard = OpenGuard::new(self.0.datalith.clone(), file_id).await;
        let path = self.0.datalith.get_file_path(file_id).await?;
        let file = tokio::fs::File::open(path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ServiceError::NotFound
            } else {
                error.into()
            }
        })?;
        self.authorize_media(id, session).await?;
        Ok(Content {
            file,
            metadata: MediaFile {
                id:        file_id,
                sha256:    hex::encode(row.try_get::<Vec<u8>, _>("hash")?),
                file_size: row.try_get::<i64, _>("file_size")?.to_string(),
                file_type: if is_video { "video/mp4" } else { "audio/mp4" }.into(),
                file_name: name,
            },
            created_at: media.created_at,
            single_use: media.single_use,
            repeatable: true,
            temporary: media.single_use || media.expires_at.is_some(),
            _file_guard: Some(guard),
        })
    }
}

pub(super) fn target_duration(inventory: &HlsInventory) -> Result<u64, ServiceError> {
    let maximum = inventory
        .tracks
        .iter()
        .flat_map(|track| {
            track
                .segments
                .iter()
                .map(move |segment| segment.duration as f64 / f64::from(track.timescale))
        })
        .fold(0.0f64, f64::max);
    if !maximum.is_finite() || maximum <= 0.0 {
        return Err(ServiceError::Invalid("invalid HLS segment duration".into()));
    }
    Ok(maximum.ceil() as u64)
}

pub(super) fn track_playlist(track: &HlsTrack, target: u64, suffix: &str) -> String {
    let mut body = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:0\n#\
         EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4{suffix}\"\n"
    );
    for (index, segment) in track.segments.iter().enumerate() {
        body.push_str(&format!(
            "#EXTINF:{:.6},\nsegment-{index:06}.m4s{suffix}\n",
            segment.duration as f64 / f64::from(track.timescale)
        ));
    }
    body.push_str("#EXT-X-ENDLIST\n");
    body
}

fn bandwidth(track: &HlsTrack, target: u64) -> Result<(u64, u64), ServiceError> {
    let mut durations = vec![0.0];
    let mut bits = vec![0.0];
    let mut fallback = 0u64;
    for segment in &track.segments {
        let duration = segment.duration as f64 / f64::from(track.timescale);
        let bytes: u64 = segment
            .file
            .file_size
            .parse()
            .map_err(|_| ServiceError::Invalid("invalid HLS file size".into()))?;
        if duration <= 0.0 || !duration.is_finite() {
            return Err(ServiceError::Invalid("invalid HLS duration".into()));
        }
        durations.push(durations.last().unwrap() + duration);
        bits.push(bits.last().unwrap() + bytes as f64 * 8.0);
        fallback = fallback.max((bytes as f64 * 8.0 / duration).ceil() as u64);
    }
    let average = (bits.last().unwrap() / durations.last().unwrap()).ceil() as u64;
    let minimum = target as f64 * 0.5;
    let maximum = target as f64 * 1.5;
    let test = |rate: u64| {
        let mut queue = VecDeque::<usize>::new();
        let mut next = 0usize;
        let mut found = false;
        let value = |index: usize| bits[index] - rate as f64 * durations[index];
        for end in 1..durations.len() {
            while next < end && durations[end] - durations[next] >= minimum {
                while queue.back().is_some_and(|&index| value(index) >= value(next)) {
                    queue.pop_back();
                }
                queue.push_back(next);
                next += 1;
            }
            while queue.front().is_some_and(|&index| durations[end] - durations[index] > maximum) {
                queue.pop_front();
            }
            if let Some(&start) = queue.front() {
                found = true;
                if value(end) - value(start) > 0.000001 {
                    return (true, true);
                }
            }
        }
        (false, found)
    };
    if !test(fallback).1 {
        return Ok((fallback.max(average), average));
    }
    let mut low = 0;
    let mut high = fallback;
    while low < high {
        let middle = low + (high - low) / 2;
        if test(middle).0 {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    Ok((high.max(average), average))
}

pub(super) fn validate_audio(
    audio: &super::AudioVariant,
    standalone: bool,
) -> Result<(), ServiceError> {
    let invalid = || ServiceError::Invalid("invalid audio metadata".into());
    match (audio.id.as_str(), audio.codec.as_str()) {
        ("aac_low" | "aac_high", "aac") => {
            if audio.sample_rate != 48_000
                || !(1..=2).contains(&audio.channels)
                || audio.bits_per_sample.is_some()
            {
                return Err(invalid());
            }
        },
        ("flac", "flac") => {
            if audio.sample_rate == 0
                || audio.sample_rate > 655_350
                || !(1..=8).contains(&audio.channels)
                || audio.bits_per_sample.is_none_or(|bits| !(4..=32).contains(&bits))
            {
                return Err(invalid());
            }
        },
        _ => return Err(invalid()),
    }
    if audio.bitrate == 0 || audio.file.is_some() != standalone {
        return Err(invalid());
    }
    if let Some(file) = &audio.file
        && file.file_type != if audio.codec == "flac" { "audio/flac" } else { "audio/mp4" }
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn validate_video(
    media: &super::VideoMedia,
    inventory: &HlsInventory,
) -> Result<(), ServiceError> {
    use std::collections::HashSet;
    let invalid = || ServiceError::Invalid("invalid HLS metadata".into());
    if !media.duration_seconds.is_finite()
        || media.duration_seconds <= 0.0
        || inventory.timescale != 120_000
        || inventory.duration == 0
        || inventory.presentation_start < 0
    {
        return Err(invalid());
    }
    if (media.duration_seconds - inventory.duration as f64 / f64::from(inventory.timescale)).abs()
        > 1.0 / f64::from(inventory.timescale)
        || inventory
            .presentation_start
            .checked_add(i64::try_from(inventory.duration).map_err(|_| invalid())?)
            .is_none()
    {
        return Err(invalid());
    }
    let audio: HashMap<_, _> =
        media.audio.iter().map(|output| (output.id.as_str(), output)).collect();
    if audio.len() != media.audio.len() || !audio.is_empty() && !audio.contains_key("aac_low") {
        return Err(invalid());
    }
    for output in &media.audio {
        validate_audio(output, false)?;
    }
    let canvases = [
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
    for variant in &media.variants {
        if !safe_id(&variant.id)
            || !h264_codec(&variant.codec)
            || !canvases.contains(&(variant.resolution, variant.width, variant.height))
            || ![10, 12, 15, 20, 24, 25, 30, 48, 50, 60].contains(&variant.fps)
            || variant.frame_rate.numerator == 0
            || variant.frame_rate.denominator == 0
            || !variant.leading_hold_seconds.is_finite()
            || variant.leading_hold_seconds < 0.0
        {
            return Err(invalid());
        }
        let rate =
            f64::from(variant.frame_rate.numerator) / f64::from(variant.frame_rate.denominator);
        if (rate - f64::from(variant.fps)).abs() > 0.000001
            && (![24, 30, 60].contains(&variant.fps)
                || (rate - f64::from(variant.fps) * 1000.0 / 1001.0).abs() > 0.000001)
        {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        if !audio.is_empty() && !variant.audio.iter().any(|id| id == "aac_low") {
            return Err(invalid());
        }
        for id in &variant.audio {
            if !ids.insert(id)
                || !audio.contains_key(id.as_str())
                || id == "aac_high" && variant.resolution < 720
                || id == "flac" && variant.resolution < 1080
            {
                return Err(invalid());
            }
        }
    }
    for track in &inventory.tracks {
        if !safe_id(&track.id)
            || track.timescale == 0
            || track.presentation_start < 0
            || track.presentation_end <= track.presentation_start
            || track.segments.is_empty()
        {
            return Err(invalid());
        }
        let is_video = media.variants.iter().find(|variant| variant.id == track.id);
        if let Some(variant) = is_video {
            if track.timescale != 120_000
                || track.codec != variant.codec
                || track.skip_samples != 0
                || track.discard_padding != 0
            {
                return Err(invalid());
            }
            if track.presentation_start.abs_diff(inventory.presentation_start) > 1 {
                return Err(invalid());
            }
        } else {
            let audio = audio.get(track.id.as_str()).ok_or_else(invalid)?;
            if track.timescale != audio.sample_rate
                || track.codec != if audio.codec == "aac" { "mp4a.40.2" } else { "fLaC" }
            {
                return Err(invalid());
            }
        }
        let mime = if is_video.is_some() { "video/mp4" } else { "audio/mp4" };
        if track.initialization.file_type != mime {
            return Err(invalid());
        }
        let mut previous = None;
        let mut segment_start = i64::MAX;
        let mut segment_end = i64::MIN;
        for segment in &track.segments {
            let end = segment
                .start
                .checked_add(i64::try_from(segment.duration).map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
            if segment.start < 0
                || segment.duration == 0
                || segment.file.file_type != mime
                || is_video.is_some() && !segment.independent
                || previous.is_some_and(|start| segment.start <= start)
                || end <= segment.start
            {
                return Err(invalid());
            }
            previous = Some(segment.start);
            segment_start = segment_start.min(segment.start);
            segment_end = segment_end.max(end);
        }
        if is_video.is_some()
            && (track.presentation_start.abs_diff(segment_start) > 1
                || track.presentation_end.abs_diff(segment_end) > 1)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn h264_codec(codec: &str) -> bool {
    let Some(bytes) = codec
        .strip_prefix("avc1.")
        .filter(|hex| hex.len() == 6)
        .and_then(|hex| hex::decode(hex).ok())
    else {
        return false;
    };
    matches!(bytes[0], 66 | 77 | 100)
        && [10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52].contains(&bytes[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::HlsSegment;

    fn file(size: u64) -> MediaFile {
        MediaFile {
            id:        Uuid::nil(),
            sha256:    "00".repeat(32),
            file_size: size.to_string(),
            file_type: "video/mp4".into(),
            file_name: "segment.m4s".into(),
        }
    }

    #[test]
    fn bandwidth_uses_eligible_windows_and_excludes_initialization_bytes() {
        let track = HlsTrack {
            id:                 "video".into(),
            initialization:     file(10_000_000),
            segments:           vec![
                HlsSegment {
                    file:        file(1000),
                    start:       0,
                    duration:    720_000,
                    independent: true,
                },
                HlsSegment {
                    file:        file(1000),
                    start:       720_000,
                    duration:    720_000,
                    independent: true,
                },
                HlsSegment {
                    file:        file(1000),
                    start:       1_440_000,
                    duration:    12_000,
                    independent: true,
                },
            ],
            timescale:          120_000,
            codec:              "avc1.640028".into(),
            average_bandwidth:  0,
            peak_bandwidth:     0,
            presentation_start: 0,
            presentation_end:   1_452_000,
            skip_samples:       0,
            discard_padding:    0,
        };
        assert_eq!((2623, 1984), bandwidth(&track, 6).unwrap());
        let mut short = track;
        short.segments = vec![HlsSegment {
            file:        file(1000),
            start:       0,
            duration:    120_000,
            independent: true,
        }];
        assert_eq!((8000, 8000), bandwidth(&short, 6).unwrap());
    }
}
