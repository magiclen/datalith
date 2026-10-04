pub(super) mod executor;
mod mp4;
mod probe;
mod video;

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use executor::{ffmpeg_options, input_options, push};
use probe::{PacketStats, Probe, Stream};
use tokio::fs;
use uuid::Uuid;

use super::{
    AudioMedia, AudioVariant, DatalithService, HlsInventory, HlsSegment, HlsTrack, MediaKind,
    PreparedFile, ProcessingMethod, ProcessingMode, ProcessingRecipe, ProcessingWarning,
    ServiceError, UploadOptions, VideoMedia, VideoVariant,
};

pub(super) struct Processed {
    pub original_mime: String,
    pub audio:         Option<AudioMedia>,
    pub video:         Option<VideoMedia>,
    pub inventory:     Option<HlsInventory>,
    pub warnings:      Vec<ProcessingWarning>,
    pub prepared:      HashMap<Uuid, PreparedFile>,
}

struct EncodedAudio {
    id:      String,
    path:    PathBuf,
    codec:   String,
    method:  ProcessingMethod,
    stream:  Stream,
    packets: PacketStats,
}
impl EncodedAudio {
    fn summary(&self, id: Uuid) -> AudioVariant {
        let rate = self.stream.number("sample_rate") as u32;
        AudioVariant {
            id:                self.id.clone(),
            codec:             self.codec.clone(),
            bitrate:           self.packets.bitrate(rate),
            sample_rate:       rate,
            channels:          self.stream.number("channels") as u16,
            bits_per_sample:   if self.codec == "flac" {
                Some(self.stream.bit_depth())
            } else {
                None
            },
            processing_method: self.method,
            file:              None,
            content_path:      format!("api/v1/media/{id}/hls/{}/index.m3u8", self.id),
        }
    }
}

struct Pipeline<'a> {
    service:       &'a DatalithService,
    task:          Uuid,
    input:         &'a Path,
    output:        &'a Path,
    cancel:        &'a AtomicBool,
    completed:     u64,
    total:         u64,
    source_format: String,
}
struct VideoTiming {
    origin:       f64,
    epoch:        f64,
    duration:     f64,
    leading_hold: f64,
}
struct AudioPolicy {
    mode:         ProcessingMode,
    lossless:     bool,
    hls:          bool,
    high_allowed: bool,
}
impl Pipeline<'_> {
    async fn run(
        &mut self,
        args: &[OsString],
        stage: &str,
        duration: f64,
    ) -> Result<(), ServiceError> {
        self.service.processing_progress(self.task, stage, self.completed, self.total).await?;
        self.service
            .0
            .av
            .run(
                &self.service.0.config.av.ffmpeg,
                args,
                self.cancel,
                Some((self.service, self.task, stage, self.completed, self.total)),
                false,
                |_| Ok(()),
            )
            .await?;
        self.completed = self.completed.saturating_add((duration * 1000.0).ceil() as u64);
        self.service.processing_progress(self.task, stage, self.completed, self.total).await
    }

    async fn audio_file(
        &mut self,
        source: &Stream,
        codec: &str,
        bitrate: u64,
        copy: bool,
        duration: f64,
    ) -> Result<EncodedAudio, ServiceError> {
        let id = if codec == "flac" {
            "flac"
        } else if bitrate <= 128_000 {
            "aac_low"
        } else {
            "aac_high"
        };
        let path =
            self.output.join(format!("{id}.{}", if codec == "flac" { "flac" } else { "m4a" }));
        let mut args = ffmpeg_options();
        if copy {
            push(&mut args, &[
                "-copyts",
                "-itsoffset",
                &format!("{:.9}", -source.number("start_time")),
            ]);
        }
        args.extend(input_options());
        if self.source_format.split(',').any(|name| name == "mov") {
            push(&mut args, &["-enable_drefs", "0", "-use_absolute_path", "0"]);
        }
        push(&mut args, &["-i"]);
        args.push(self.input.as_os_str().into());
        push(&mut args, &[
            "-map",
            &format!("0:{}", source.index()),
            "-vn",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
        ]);
        if copy {
            push(&mut args, &["-c:a", "copy", "-avoid_negative_ts", "disabled"]);
        } else if codec == "aac" {
            push(&mut args, &[
                "-af",
                "asetpts=PTS-STARTPTS",
                "-c:a",
                "aac",
                "-profile:a",
                "aac_low",
                "-aac_coder",
                "twoloop",
                "-ar",
                "48000",
                "-ac",
                &source.number("channels").clamp(1.0, 2.0).to_string(),
                "-b:a",
                &bitrate.to_string(),
            ]);
        } else {
            push(&mut args, &[
                "-af",
                "asetpts=PTS-STARTPTS",
                "-c:a",
                "flac",
                "-compression_level",
                "12",
                "-sample_fmt",
                if source.bit_depth() <= 16 { "s16" } else { "s32" },
                "-bits_per_raw_sample",
                &source.bit_depth().to_string(),
            ]);
            if source.bit_depth() == 32 {
                push(&mut args, &["-strict", "experimental"]);
            }
        }
        push(&mut args, &["-threads", &self.service.0.config.av.encoder_threads.to_string()]);
        if codec == "aac" {
            push(&mut args, &["-movflags", "+faststart", "-f", "ipod"]);
        } else {
            push(&mut args, &["-f", "flac"]);
        }
        args.push(path.as_os_str().into());
        self.run(&args, "processing_audio", duration).await?;
        let probe = probe::inspect(self.service, &path, self.cancel).await?;
        let stream = probe
            .audio(None)?
            .ok_or_else(|| ServiceError::Internal("encoded audio has no audio stream".into()))?
            .clone();
        let packets = probe::packets(self.service, &path, &stream, self.cancel, None).await?;
        Ok(EncodedAudio {
            id: id.into(),
            path,
            codec: codec.into(),
            method: if copy { ProcessingMethod::Remuxed } else { ProcessingMethod::Transcoded },
            stream,
            packets,
        })
    }

    async fn audio_versions(
        &mut self,
        probe: &Probe,
        source: &Stream,
        stats: &PacketStats,
        policy: AudioPolicy,
    ) -> Result<(Vec<EncodedAudio>, Vec<ProcessingWarning>), ServiceError> {
        let AudioPolicy {
            mode,
            lossless,
            hls,
            high_allowed,
        } = policy;
        let rate = source.number("sample_rate") as u32;
        let duration = if source.number("duration") > 0.0 {
            source.number("duration")
        } else {
            stats.duration(rate)
        };
        let bitrate = stats.bitrate(rate);
        let compliant = mode == ProcessingMode::Trust
            && source.text("codec_name") == "aac"
            && source.text("profile") == "LC"
            && rate == 48_000
            && (1..=2).contains(&(source.number("channels") as u16))
            && bitrate <= 256_000;
        let mut audio = Vec::new();
        let mut warnings = Vec::new();
        let high = if hls && !high_allowed {
            self.audio_file(source, "aac", 128_000, compliant && bitrate <= 128_000, duration)
                .await?
        } else if compliant {
            self.audio_file(
                source,
                "aac",
                if bitrate <= 128_000 { 128_000 } else { 256_000 },
                true,
                duration,
            )
            .await?
        } else {
            self.audio_file(source, "aac", 256_000, false, duration).await?
        };
        let high_accepted =
            high.id == "aac_high" && high.packets.payload_bytes <= stats.payload_bytes;
        if hls {
            if high_accepted || high.id == "aac_low" {
                audio.push(high);
            } else {
                fs::remove_file(&high.path).await?;
            }
            if !audio.iter().any(|audio| audio.id == "aac_low") {
                audio.push(
                    self.audio_file(
                        source,
                        "aac",
                        128_000,
                        compliant && bitrate <= 128_000,
                        duration,
                    )
                    .await?,
                );
            }
        } else if high_accepted || high.id == "aac_low" {
            audio.push(high);
        } else {
            fs::remove_file(&high.path).await?;
            audio.push(self.audio_file(source, "aac", 128_000, false, duration).await?);
        }
        if lossless && source.lossless(&self.service.0.av) {
            if source.flac_compatible(&self.service.0.av)
                && (self.service.0.av.flac_available
                    || mode == ProcessingMode::Trust && source.text("codec_name") == "flac")
            {
                match self
                    .audio_file(
                        source,
                        "flac",
                        0,
                        mode == ProcessingMode::Trust && source.text("codec_name") == "flac",
                        duration,
                    )
                    .await
                {
                    Ok(flac) => audio.insert(0, flac),
                    Err(ServiceError::Invalid(message)) => warnings.push(ProcessingWarning {
                        code:    "lossless_not_preserved".into(),
                        message: format!("FLAC could not preserve the source audio: {message}"),
                    }),
                    Err(error) => return Err(error),
                }
            } else {
                warnings.push(ProcessingWarning {
                    code:    "lossless_not_preserved".into(),
                    message: "The source sample format cannot be preserved in FLAC.".into(),
                });
            }
        }
        if lossless && self.service.0.av.mixed_lossless.contains(source.text("codec_name")) {
            warnings.push(ProcessingWarning {
                code:    "lossless_not_preserved".into(),
                message: "The source codec can be lossy or lossless, and its lossless mode could \
                          not be verified."
                    .into(),
            });
        }
        if !hls
            && mode == ProcessingMode::Trust
            && probe.streams.len() == 1
            && source.number("start_time").abs() < 0.00001
        {
            for output in &mut audio {
                let format = probe.format["format_name"].as_str().unwrap_or("");
                let direct = output.method == ProcessingMethod::Remuxed
                    && (output.codec == "flac" && format == "flac"
                        || output.codec == "aac" && format.split(',').any(|name| name == "mov"));
                if direct {
                    fs::remove_file(&output.path).await?;
                    output.path = self.input.into();
                    output.method = ProcessingMethod::Copied;
                }
            }
        }
        Ok((audio, warnings))
    }

    async fn segment_audio(
        &mut self,
        audio: &EncodedAudio,
        offset: f64,
        epoch: f64,
        presentation_end: f64,
        prepared: &mut HashMap<Uuid, PreparedFile>,
    ) -> Result<HlsTrack, ServiceError> {
        let directory = self.output.join(&audio.id);
        fs::create_dir(&directory).await?;
        let mut args = ffmpeg_options();
        push(&mut args, &["-copyts", "-itsoffset", &format!("{offset:.9}")]);
        args.extend(input_options());
        if audio.codec == "aac" {
            push(&mut args, &["-enable_drefs", "0", "-use_absolute_path", "0"]);
        }
        push(&mut args, &["-i"]);
        args.push(audio.path.as_os_str().into());
        push(&mut args, &[
            "-map",
            "0:a:0",
            "-c:a",
            "copy",
            "-vn",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-output_ts_offset",
            &format!("{epoch:.9}"),
        ]);
        if audio.codec == "flac" {
            push(&mut args, &["-strict", "experimental"]);
        }
        hls_options(&mut args, &directory, false);
        self.run(
            &args,
            "segmenting_audio",
            audio.packets.duration(audio.stream.number("sample_rate") as u32),
        )
        .await?;
        let rate = audio.stream.number("sample_rate") as u32;
        let codec = if audio.codec == "aac" { "mp4a.40.2" } else { "fLaC" };
        let mut track =
            inventory_track(self.service, &directory, &audio.id, rate, codec, false, prepared)
                .await?;
        track.presentation_start =
            ((epoch + offset + audio.packets.start(rate)) * f64::from(rate)).round() as i64;
        track.presentation_end = (presentation_end * f64::from(rate)).round() as i64;
        track.skip_samples = audio.packets.skip_samples;
        track.discard_padding = audio.packets.discard_padding;
        Ok(track)
    }

    async fn segment_video(
        &mut self,
        stream: &Stream,
        rendition: &video::Rendition,
        timing: &VideoTiming,
        copy: bool,
        prepared: &mut HashMap<Uuid, PreparedFile>,
    ) -> Result<(VideoVariant, HlsTrack), ServiceError> {
        let VideoTiming {
            origin,
            epoch,
            duration,
            leading_hold,
        } = *timing;
        let id = rendition.id();
        let directory = self.output.join(&id);
        fs::create_dir(&directory).await?;
        let mut args = ffmpeg_options();
        push(&mut args, &["-copyts"]);
        if copy {
            push(&mut args, &["-itsoffset", &format!("{:.9}", -origin)]);
        }
        args.extend(input_options());
        if self.source_format.split(',').any(|name| name == "mov") {
            push(&mut args, &["-enable_drefs", "0", "-use_absolute_path", "0"]);
        }
        push(&mut args, &["-i"]);
        args.push(self.input.as_os_str().into());
        push(&mut args, &[
            "-map",
            &format!("0:{}", stream.index()),
            "-an",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
        ]);
        if copy {
            push(&mut args, &["-c:v", "copy"]);
        } else {
            let level = format!("{}.{}", rendition.level.code / 10, rendition.level.code % 10);
            push(&mut args, &[
                "-vf",
                &video::filters(stream, rendition, origin),
                "-c:v",
                "libx264",
                "-preset",
                "veryslow",
                "-crf",
                "23",
                "-profile:v",
                "high",
                "-level:v",
                &level,
                "-pix_fmt",
                "yuv420p",
                "-maxrate",
                &rendition.maxrate.to_string(),
                "-bufsize",
                &rendition.maxrate.saturating_mul(2).to_string(),
                "-refs",
                &rendition.references.to_string(),
                "-bf",
                &(rendition.references.min(4).saturating_sub(1)).to_string(),
                "-flags",
                "+cgop",
                "-x264-params",
                "open-gop=0:scenecut=0",
                "-forced-idr",
                "1",
                "-force_key_frames",
                "expr:gte(t,n_forced*2)",
                "-threads",
                &self.service.0.config.av.encoder_threads.to_string(),
                "-filter_threads",
                &self.service.0.config.av.encoder_threads.to_string(),
            ]);
            if stream.hdr() {
                push(&mut args, &[
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                ]);
            }
        }
        if leading_hold > 0.0 {
            push(&mut args, &[
                "-bsf:v",
                &format!(
                    "setts=pts='PTS-if(eq(N,0),{leading_hold:.9}/TB,0)':dts='DTS-if(eq(N,0),\
                     {leading_hold:.9}/TB,0)':duration='DURATION+if(eq(N,0),{leading_hold:.9}/TB,\
                     0)':time_base=1/120000:prescale=1"
                ),
            ]);
        }
        push(&mut args, &["-output_ts_offset", &format!("{epoch:.9}")]);
        hls_options(&mut args, &directory, true);
        self.run(&args, "processing_video", duration).await?;
        let initialization =
            probe::inspect(self.service, &directory.join("init.mp4"), self.cancel).await?;
        let actual = initialization
            .streams
            .iter()
            .find(|stream| stream.text("codec_type") == "video")
            .ok_or_else(|| {
                ServiceError::Internal("video initialization has no video stream".into())
            })?;
        let info = actual.h264().ok_or_else(|| {
            ServiceError::Internal("video initialization has no H.264 configuration".into())
        })?;
        if !info.progressive
            || info.level > rendition.level.code
            || info.references.max(info.buffering.unwrap_or(0)).saturating_mul(info.macroblocks)
                > rendition.level.dpb
        {
            return Err(ServiceError::Internal(
                "encoded video exceeds its H.264 decoding limits".into(),
            ));
        }
        let track =
            inventory_track(self.service, &directory, &id, 120_000, &info.codec, true, prepared)
                .await?;
        let summary = VideoVariant {
            id:                   id.clone(),
            resolution:           rendition.resolution,
            width:                rendition.width,
            height:               rendition.height,
            fps:                  rendition.fps,
            frame_rate:           rendition.rate,
            leading_hold_seconds: leading_hold,
            codec:                info.codec,
            processing_method:    if copy {
                ProcessingMethod::Remuxed
            } else {
                ProcessingMethod::Transcoded
            },
            playlist_path:        format!("api/v1/media/{}/hls/{id}/index.m3u8", self.task),
            audio:                Vec::new(),
        };
        Ok((summary, track))
    }
}

pub(super) fn validate_options(options: &UploadOptions) -> Result<(), ServiceError> {
    if options.kind == MediaKind::Video {
        if options.video.variants.is_empty() || options.video.variants.len() > 16 {
            return Err(ServiceError::Invalid(
                "video variants must contain 1 to 16 resolution and frame-rate pairs".into(),
            ));
        }
        for variant in &options.video.variants {
            if !video::RESOLUTIONS.iter().any(|(tier, ..)| *tier == variant.resolution)
                || !video::FRAME_RATES.contains(&variant.fps)
            {
                return Err(ServiceError::Invalid(
                    "unsupported video resolution or frame-rate tier".into(),
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn process(
    service: &DatalithService,
    task: Uuid,
    input: &Path,
    output: &Path,
    options: &UploadOptions,
    recipe: &ProcessingRecipe,
    cancel: &AtomicBool,
) -> Result<Processed, ServiceError> {
    if recipe.version != 1 {
        return Err(ServiceError::Unsupported("unsupported processing recipe version".into()));
    }
    if !service.0.av.available {
        return Err(ServiceError::Unsupported(
            service
                .0
                .av
                .reason
                .clone()
                .unwrap_or_else(|| "audio and video tools are unavailable".into()),
        ));
    }
    validate_options(options)?;
    let probe = probe::inspect(service, input, cancel).await?;
    let audio_options = if options.kind == MediaKind::Video {
        (options.video.processing_mode, options.video.preserve_lossless, options.video.audio_stream)
    } else {
        (options.audio.processing_mode, options.audio.preserve_lossless, options.audio.audio_stream)
    };
    let source_audio = probe.audio(audio_options.2)?;
    let audio_stats = if let Some(audio) = source_audio {
        Some(probe::packets(service, input, audio, cancel, None).await?)
    } else {
        None
    };
    let duration = probe.format["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(
            audio_stats
                .as_ref()
                .zip(source_audio)
                .map_or(0.0, |(stats, stream)| stats.duration(stream.number("sample_rate") as u32)),
        )
        .max(0.001);
    let mut source_video = if options.kind == MediaKind::Video {
        Some(
            probe
                .streams
                .iter()
                .find(|stream| {
                    stream.text("codec_type") == "video"
                        && stream.0["disposition"]["attached_pic"].as_i64() != Some(1)
                })
                .ok_or_else(|| ServiceError::Invalid("the source has no video stream".into()))?
                .clone(),
        )
    } else {
        None
    };
    let video_stats = if let Some(stream) = &source_video {
        Some(probe::packets(service, input, stream, cancel, None).await?)
    } else {
        None
    };
    if video_stats.as_ref().is_some_and(|stats| stats.cadence) {
        source_video.as_mut().unwrap().0["_nominal_cadence_verified"] = true.into();
    }
    let renditions = source_video
        .as_ref()
        .map(|stream| video::renditions(&options.video, stream, recipe.video_bitrate))
        .transpose()?
        .unwrap_or_default();
    let mut pipeline = Pipeline {
        service,
        task,
        input,
        output,
        cancel,
        completed: 0,
        total: ((duration * 1000.0).ceil() as u64)
            .saturating_mul(options.video.variants.len() as u64 + 8),
        source_format: probe.format["format_name"].as_str().unwrap_or("").into(),
    };
    let mut prepared = HashMap::new();
    let mut warnings = Vec::new();
    let mut encoded_audio = if let Some((audio, stats)) = source_audio.zip(audio_stats.as_ref()) {
        let (audio, reported) = pipeline
            .audio_versions(&probe, audio, stats, AudioPolicy {
                mode:         audio_options.0,
                lossless:     audio_options.1
                    && (options.kind == MediaKind::Audio
                        || renditions.iter().any(|rendition| rendition.resolution >= 1080)),
                hls:          options.kind == MediaKind::Video,
                high_allowed: options.kind == MediaKind::Audio
                    || renditions.iter().any(|rendition| rendition.resolution >= 720),
            })
            .await?;
        warnings = reported;
        audio
    } else {
        Vec::new()
    };
    let original_mime = crate::functions::detect_file_type_by_path(input, false)
        .await
        .map(|mime| mime.to_string())
        .unwrap_or_else(|| original_mime(&probe, options.kind));
    if options.kind == MediaKind::Audio {
        if encoded_audio.is_empty() {
            return Err(ServiceError::Invalid("the source has no audio stream".into()));
        }
        let mut variants = Vec::new();
        for audio in &mut encoded_audio {
            let format = if audio.codec == "flac" { "flac" } else { "m4a" };
            let file = DatalithService::prepare_file(
                audio.path.clone(),
                if format == "flac" { "audio/flac" } else { "audio/mp4" }.into(),
                format!(
                    "{}-{}.{format}",
                    options.file_name.as_deref().unwrap_or("audio"),
                    audio.id
                ),
            )
            .await?;
            let mut summary = audio.summary(task);
            summary.file = Some(file.metadata.clone());
            summary.content_path = format!("api/v1/media/{task}/content?format={format}");
            variants.push(summary);
            prepared.insert(file.metadata.id, file);
        }
        let audio_duration =
            source_audio.zip(audio_stats.as_ref()).map_or(duration, |(stream, stats)| {
                stats.duration(stream.number("sample_rate") as u32)
            });
        return Ok(Processed {
            original_mime,
            audio: Some(AudioMedia {
                duration_seconds: audio_duration,
                variants,
            }),
            video: None,
            inventory: None,
            warnings,
            prepared,
        });
    }
    let stream = source_video.as_ref().unwrap();
    let stats = video_stats.as_ref().unwrap();
    let origin = stats.first_pts.unwrap();
    let earliest_dts = audio_stats
        .as_ref()
        .and_then(|stats| stats.first_dts)
        .unwrap_or(origin)
        .min(stats.first_dts.unwrap_or(origin));
    let epoch = (origin - earliest_dts).max(0.0).ceil() + 1.0;
    let audio_start = source_audio
        .zip(audio_stats.as_ref())
        .map_or(origin, |(stream, stats)| stats.start(stream.number("sample_rate") as u32));
    let end = stats.end.max(audio_stats.as_ref().map_or(stats.end, |stats| stats.end));
    let start = origin.min(audio_start);
    let presentation_duration = end - start;
    let mut tracks = Vec::new();
    let mut summaries = Vec::new();
    for rendition in &renditions {
        let copy = if options.video.processing_mode == ProcessingMode::Trust
            && probe.format["format_name"]
                .as_str()
                .is_some_and(|format| format.split(',').any(|name| name == "mov"))
            && video::can_copy(stream, rendition)
        {
            let verified = probe::packets(
                service,
                input,
                stream,
                cancel,
                Some((rendition.maxrate, rendition.frame_duration(), origin)),
            )
            .await?;
            verified.regular
                && verified.envelope
                && verified.aligned_keys
                && verified.bitrate(1) <= rendition.maxrate
        } else {
            false
        };
        let (mut summary, track) = pipeline
            .segment_video(
                stream,
                rendition,
                &VideoTiming {
                    origin,
                    epoch,
                    duration: stats.end - origin,
                    leading_hold: origin - start,
                },
                copy,
                &mut prepared,
            )
            .await?;
        summary.audio = encoded_audio
            .iter()
            .filter(|audio| {
                audio.id == "aac_low"
                    || audio.id == "aac_high" && rendition.resolution >= 720
                    || audio.codec == "flac" && rendition.resolution >= 1080
            })
            .map(|audio| audio.id.clone())
            .collect();
        tracks.push(track);
        summaries.push(summary);
    }
    encoded_audio.retain(|audio| summaries.iter().any(|video| video.audio.contains(&audio.id)));
    let mut audio_summaries = Vec::new();
    for audio in &encoded_audio {
        let offset = audio_start - origin;
        let source_end = audio_stats.as_ref().map_or(end, |stats| stats.end);
        tracks.push(
            pipeline
                .segment_audio(audio, offset, epoch, epoch + source_end - origin, &mut prepared)
                .await?,
        );
        audio_summaries.push(audio.summary(task));
    }
    if cancel.load(Ordering::Acquire) {
        return Err(ServiceError::Cancelled);
    }
    Ok(Processed {
        original_mime,
        audio: None,
        video: Some(VideoMedia {
            duration_seconds: presentation_duration,
            variants:         summaries,
            audio:            audio_summaries,
            master_path:      format!("api/v1/media/{task}/hls/master.m3u8"),
        }),
        inventory: Some(HlsInventory {
            presentation_start: ((epoch + start - origin) * 120_000.0).round() as i64,
            duration: (presentation_duration * 120_000.0).round() as u64,
            timescale: 120_000,
            tracks,
        }),
        warnings,
        prepared,
    })
}

fn hls_options(args: &mut Vec<OsString>, directory: &Path, video: bool) {
    push(args, &[
        "-f",
        "hls",
        "-hls_time",
        "6",
        "-hls_list_size",
        "0",
        "-hls_playlist_type",
        "vod",
        "-hls_segment_type",
        "fmp4",
        "-hls_fmp4_init_filename",
        "init.mp4",
        "-hls_segment_options",
        "video_track_timescale=120000:movie_timescale=120000:avoid_negative_ts=disabled",
        "-hls_flags",
        if video { "independent_segments" } else { "temp_file" },
        "-hls_segment_filename",
    ]);
    args.push(directory.join("segment-%06d.m4s").into_os_string());
    args.push(directory.join("index.m3u8").into_os_string());
}

async fn inventory_track(
    service: &DatalithService,
    directory: &Path,
    id: &str,
    timescale: u32,
    codec: &str,
    video: bool,
    prepared: &mut HashMap<Uuid, PreparedFile>,
) -> Result<HlsTrack, ServiceError> {
    let timeline_directory = directory.to_owned();
    tokio::task::spawn_blocking(move || mp4::normalize_timeline(&timeline_directory))
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))??;
    let init = DatalithService::prepare_file(
        directory.join("init.mp4"),
        if video { "video/mp4" } else { "audio/mp4" }.into(),
        format!("{id}-init.mp4"),
    )
    .await?;
    let initialization = init.metadata.clone();
    prepared.insert(initialization.id, init);
    let playlist = fs::read_to_string(directory.join("index.m3u8")).await?;
    let mut segments = Vec::new();
    let mut bytes = 0u64;
    let mut duration = 0u64;
    let mut peak = 0u64;
    for name in playlist.lines().filter(|line| !line.starts_with('#') && !line.is_empty()) {
        if !name.starts_with("segment-")
            || !name.ends_with(".m4s")
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(ServiceError::Internal("unexpected HLS segment name".into()));
        }
        let path = directory.join(name);
        let timing_path = path.clone();
        let timing = tokio::task::spawn_blocking(move || mp4::fragment_timing(&timing_path))
            .await
            .map_err(|error| ServiceError::Internal(error.to_string()))??;
        let file = DatalithService::prepare_file(
            path,
            if video { "video/mp4" } else { "audio/mp4" }.into(),
            format!("{id}-{name}"),
        )
        .await?;
        let size = file
            .metadata
            .file_size
            .parse::<u64>()
            .map_err(|_| ServiceError::Internal("invalid generated file size".into()))?;
        let ticks = (timing.end - timing.start) as u64;
        peak = peak.max((size as f64 * 8.0 * f64::from(timescale) / ticks as f64).ceil() as u64);
        bytes = bytes.saturating_add(size);
        duration = duration.saturating_add(ticks);
        segments.push(HlsSegment {
            file:        file.metadata.clone(),
            start:       timing.start,
            duration:    ticks,
            independent: true,
        });
        prepared.insert(file.metadata.id, file);
    }
    if segments.is_empty() {
        return Err(ServiceError::Internal("HLS output has no segments".into()));
    }
    let presentation_start = segments.iter().map(|segment| segment.start).min().unwrap();
    let presentation_end =
        segments.iter().map(|segment| segment.start + segment.duration as i64).max().unwrap();
    let average = (bytes as f64 * 8.0 * f64::from(timescale) / duration as f64).ceil() as u64;
    let _ = service;
    Ok(HlsTrack {
        id: id.into(),
        initialization,
        segments,
        timescale,
        codec: codec.into(),
        average_bandwidth: average,
        peak_bandwidth: peak.max(average),
        presentation_start,
        presentation_end,
        skip_samples: 0,
        discard_padding: 0,
    })
}

fn original_mime(probe: &Probe, kind: MediaKind) -> String {
    let format = probe.format["format_name"].as_str().unwrap_or("");
    if format.split(',').any(|name| name == "mov") {
        if kind == MediaKind::Audio { "audio/mp4" } else { "video/mp4" }
    } else if format == "wav" {
        "audio/wav"
    } else if format == "flac" {
        "audio/flac"
    } else if format == "mp3" {
        "audio/mpeg"
    } else if format.contains("webm") {
        if kind == MediaKind::Audio { "audio/webm" } else { "video/webm" }
    } else if format == "ogg" {
        if kind == MediaKind::Audio { "audio/ogg" } else { "video/ogg" }
    } else {
        "application/octet-stream"
    }
    .into()
}
