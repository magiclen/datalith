use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{Semaphore, mpsc},
};
use uuid::Uuid;

use super::super::{AvConfig, DatalithService, ServiceError};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const ERROR_LIMIT: usize = 32 * 1024;

pub(in crate::service) struct Executor {
    pub available:       bool,
    pub audio_available: bool,
    pub video_available: bool,
    pub flac_available:  bool,
    pub pure_lossless:   HashSet<String>,
    pub mixed_lossless:  HashSet<String>,
    pub reason:          Option<String>,
    pub slots:           Semaphore,
    shutdown:            Arc<AtomicBool>,
}

impl Executor {
    pub async fn discover(config: &AvConfig, shutdown: Arc<AtomicBool>) -> Self {
        let executor = Self {
            available: false,
            audio_available: false,
            video_available: false,
            flac_available: false,
            pure_lossless: HashSet::new(),
            mixed_lossless: HashSet::new(),
            reason: None,
            slots: Semaphore::new(config.max_processes),
            shutdown,
        };
        let cancel = AtomicBool::new(false);
        let result = async {
            let version = executor
                .run(&config.ffmpeg, &["-version".into()], &cancel, None, true, |_| Ok(()))
                .await?;
            require_version(&version, "FFmpeg")?;
            let timestamp_filter = executor
                .run(
                    &config.ffmpeg,
                    &["-hide_banner".into(), "-h".into(), "bsf=setts".into()],
                    &cancel,
                    None,
                    true,
                    |_| Ok(()),
                )
                .await?;
            let encoders = executor
                .run(
                    &config.ffmpeg,
                    &["-hide_banner".into(), "-encoders".into()],
                    &cancel,
                    None,
                    true,
                    |_| Ok(()),
                )
                .await?;
            let version = executor
                .run(&config.ffprobe, &["-version".into()], &cancel, None, true, |_| Ok(()))
                .await?;
            require_version(&version, "ffprobe")?;
            let codecs = executor
                .run(
                    &config.ffmpeg,
                    &["-hide_banner".into(), "-codecs".into()],
                    &cancel,
                    None,
                    true,
                    |_| Ok(()),
                )
                .await?;
            let mut pure_lossless = HashSet::new();
            let mut mixed_lossless = HashSet::new();
            for line in codecs.lines() {
                let mut fields = line.split_whitespace();
                if let (Some(flags), Some(name)) = (fields.next(), fields.next())
                    && flags.as_bytes().get(2) == Some(&b'A')
                    && flags.as_bytes().get(5) == Some(&b'S')
                {
                    if flags.as_bytes().get(4) == Some(&b'L') {
                        mixed_lossless.insert(name.to_owned());
                    } else {
                        pure_lossless.insert(name.to_owned());
                    }
                }
            }
            Ok::<_, ServiceError>((
                encoders,
                timestamp_filter.contains("prescale"),
                pure_lossless,
                mixed_lossless,
            ))
        }
        .await;
        match result {
            Ok((encoders, timestamp_filter, pure_lossless, mixed_lossless)) => Self {
                available: true,
                pure_lossless,
                mixed_lossless,
                audio_available: encoders
                    .lines()
                    .any(|line| line.split_whitespace().nth(1) == Some("aac")),
                video_available: encoders
                    .lines()
                    .any(|line| line.split_whitespace().nth(1) == Some("libx264"))
                    && timestamp_filter,
                flac_available: encoders
                    .lines()
                    .any(|line| line.split_whitespace().nth(1) == Some("flac")),
                ..executor
            },
            Err(error) => Self {
                reason: Some(error.to_string()),
                ..executor
            },
        }
    }

    pub async fn run(
        &self,
        executable: &Path,
        args: &[OsString],
        cancel: &AtomicBool,
        progress: Option<(&DatalithService, Uuid, &str, u64, u64)>,
        capture: bool,
        mut consume: impl FnMut(&str) -> Result<(), ServiceError>,
    ) -> Result<String, ServiceError> {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let permit = loop {
            tokio::select! {
                permit = self.slots.acquire() => break permit.map_err(|_| ServiceError::Cancelled)?,
                _ = tick.tick() => {
                    if cancel.load(Ordering::Acquire) { return Err(ServiceError::Cancelled); }
                    if self.shutdown.load(Ordering::Acquire) { return Err(ServiceError::Busy); }
                },
            }
        };
        if cancel.load(Ordering::Acquire) {
            return Err(ServiceError::Cancelled);
        }
        let mut command = Command::new(executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id();
            // The child hook only calls system functions that are safe before exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(
                        libc::PR_SET_PDEATHSIG,
                        libc::SIGKILL as libc::c_ulong,
                        0 as libc::c_ulong,
                        0 as libc::c_ulong,
                        0 as libc::c_ulong,
                    ) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() as u32 != parent {
                        libc::kill(libc::getpid(), libc::SIGKILL);
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn().map_err(|error| {
            ServiceError::Unsupported(format!("Cannot start {}: {error}", executable.display()))
        })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ServiceError::Internal("missing child stderr".into()))?;
        let errors = tokio::spawn(async move {
            let mut reader = stderr;
            let mut tail = VecDeque::new();
            let mut buffer = [0u8; 4096];
            loop {
                let size = reader.read(&mut buffer).await?;
                if size == 0 {
                    break;
                }
                tail.extend(&buffer[..size]);
                let remove = tail.len().saturating_sub(ERROR_LIMIT);
                tail.drain(..remove);
            }
            Ok::<_, std::io::Error>(
                String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned(),
            )
        });
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ServiceError::Internal("missing child stdout".into()))?;
        let (sender, mut lines) = mpsc::channel(8);
        let output_reader = tokio::spawn(async move {
            let mut stdout = stdout;
            let mut buffer = [0u8; 4096];
            let mut line = Vec::new();
            loop {
                let count = match stdout.read(&mut buffer).await {
                    Ok(count) => count,
                    Err(error) => {
                        let _ = sender.send(Err(ServiceError::Io(error))).await;
                        return;
                    },
                };
                if count == 0 {
                    if !line.is_empty() {
                        let _ = sender
                            .send(String::from_utf8(line).map_err(|_| {
                                ServiceError::Invalid("invalid FFmpeg metadata text".into())
                            }))
                            .await;
                    }
                    return;
                }
                for &byte in &buffer[..count] {
                    if byte == b'\n' {
                        let text = String::from_utf8(std::mem::take(&mut line)).map_err(|_| {
                            ServiceError::Invalid("invalid FFmpeg metadata text".into())
                        });
                        let failed = text.is_err();
                        if sender.send(text).await.is_err() || failed {
                            return;
                        }
                    } else {
                        if line.len() >= OUTPUT_LIMIT {
                            let _ = sender
                                .send(Err(ServiceError::Invalid(
                                    "FFmpeg metadata exceeds its limit".into(),
                                )))
                                .await;
                            return;
                        }
                        line.push(byte);
                    }
                }
            }
        });
        let mut output = String::new();
        let mut out_time = 0u64;
        let mut last_progress = tokio::time::Instant::now();
        let read_result = loop {
            tokio::select! {
                line = lines.recv() => match line {
                    Some(Ok(line)) => {
                        if line.len() > OUTPUT_LIMIT || (capture && output.len().saturating_add(line.len() + 1) > OUTPUT_LIMIT) {
                            break Err(ServiceError::Invalid("FFmpeg metadata exceeds its limit".into()));
                        }
                        if capture { output.push_str(&line); output.push('\n'); }
                        if let Some(value) = line.strip_prefix("out_time_us=").and_then(|value| value.parse::<u64>().ok()) { out_time = value / 1000; }
                        if let Err(error) = consume(&line) { break Err(error); }
                    },
                    None => break Ok(()),
                    Some(Err(error)) => break Err(error),
                },
                _ = tick.tick() => {
                    if cancel.load(Ordering::Acquire) { break Err(ServiceError::Cancelled); }
                    if self.shutdown.load(Ordering::Acquire) { break Err(ServiceError::Busy); }
                    if last_progress.elapsed() >= Duration::from_secs(1) {
                        if let Some((service, id, stage, base, total)) = progress
                            && let Err(error) = service.processing_progress(id, stage, base.saturating_add(out_time), total).await {
                            break Err(error);
                        }
                        last_progress = tokio::time::Instant::now();
                    }
                },
            }
        };
        if read_result.is_err() {
            let _ = child.start_kill();
        }
        drop(lines);
        let mut interrupted = None;
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = tick.tick() => {
                    if cancel.load(Ordering::Acquire) { interrupted = Some(ServiceError::Cancelled); }
                    else if self.shutdown.load(Ordering::Acquire) { interrupted = Some(ServiceError::Busy); }
                    if interrupted.is_some() { let _ = child.start_kill(); break child.wait().await; }
                },
            }
        };
        output_reader.await.map_err(|error| ServiceError::Internal(error.to_string()))?;
        let stderr = errors.await.map_err(|error| ServiceError::Internal(error.to_string()))??;
        drop(permit);
        read_result?;
        if let Some(error) = interrupted {
            return Err(error);
        }
        let status = status?;
        if !status.success() {
            return Err(ServiceError::Invalid(format!(
                "FFmpeg processing failed: {}",
                stderr.trim()
            )));
        }
        Ok(output)
    }
}

fn require_version(output: &str, name: &str) -> Result<(), ServiceError> {
    let major = output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(2))
        .and_then(|version| version.split('.').next())
        .and_then(|value| value.parse::<u32>().ok());
    if major.is_none_or(|major| major < 9) {
        return Err(ServiceError::Unsupported(format!(
            "audio and video processing require {name} 9 or later"
        )));
    }
    Ok(())
}

pub(super) fn input_options() -> Vec<OsString> {
    [
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,avi,mpegts,mpeg,ogg,wav,w64,aiff,flac,mp3,aac,ac3,\
         eac3,ape,wv,tta,alac,amr,au,nut,flv,asf",
    ]
    .into_iter()
    .map(Into::into)
    .collect()
}

pub(super) fn ffmpeg_options() -> Vec<OsString> {
    ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats"]
        .into_iter()
        .map(Into::into)
        .collect()
}

pub(super) fn push(args: &mut Vec<OsString>, values: &[&str]) {
    args.extend(values.iter().map(Into::into));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_stops_and_reaps_an_active_process() {
        let mut config = AvConfig::default();
        if let Some(path) = std::env::var_os("DATALITH_FFMPEG") {
            config.ffmpeg = path.into();
        }
        if let Some(path) = std::env::var_os("DATALITH_FFPROBE") {
            config.ffprobe = path.into();
        }
        let shutdown = Arc::new(AtomicBool::new(false));
        let executor = Arc::new(Executor::discover(&config, shutdown.clone()).await);
        let running = executor.clone();
        let child = tokio::spawn(async move {
            running
                .run(
                    &config.ffmpeg,
                    &[
                        "-hide_banner".into(),
                        "-loglevel".into(),
                        "error".into(),
                        "-nostdin".into(),
                        "-re".into(),
                        "-f".into(),
                        "lavfi".into(),
                        "-i".into(),
                        "anullsrc=r=48000".into(),
                        "-t".into(),
                        "60".into(),
                        "-f".into(),
                        "null".into(),
                        "-".into(),
                    ],
                    &AtomicBool::new(false),
                    None,
                    false,
                    |_| Ok(()),
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        shutdown.store(true, Ordering::Release);
        let result = tokio::time::timeout(Duration::from_secs(3), child).await.unwrap().unwrap();
        assert!(matches!(result, Err(ServiceError::Busy)));
        assert_eq!(1, executor.slots.available_permits());
    }
}
