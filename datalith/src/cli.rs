use std::{net::IpAddr, path::PathBuf, time::Duration};

use byte_unit::Byte;
use clap::{Parser, Subcommand};
use datalith_core::Uuid;
use terminal_size::terminal_size;

#[derive(Debug, Parser)]
#[command(name = "Datalith", version, author)]
#[command(about = "Store files and process media with durable tasks.")]
#[command(term_width = terminal_size().map(|(width, _)| width.0 as usize).unwrap_or(0))]
pub struct CLIArgs {
    #[arg(long, visible_alias = "addr", env = "DATALITH_ADDRESS", global = true)]
    #[cfg_attr(debug_assertions, arg(default_value = "127.0.0.1"))]
    #[cfg_attr(not(debug_assertions), arg(default_value = "0.0.0.0"))]
    pub address: IpAddr,

    #[arg(long, env = "DATALITH_LISTEN_PORT", default_value = "1111", global = true)]
    pub listen_port: u16,

    #[arg(long, env = "DATALITH_ENVIRONMENT", default_value = ".", global = true)]
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub environment: PathBuf,

    #[arg(long, env = "DATALITH_MAX_FILE_SIZE", default_value = "2 GiB", global = true)]
    #[arg(help = "Maximum upload or import archive size")]
    pub max_file_size: Byte,

    #[arg(long, env = "DATALITH_TEMPORARY_FILE_LIFESPAN", default_value = "60", global = true)]
    #[arg(value_parser = parse_duration)]
    #[arg(help = "Default temporary file lifespan for the legacy Rust API in seconds")]
    pub temporary_file_lifespan: Duration,

    #[arg(long, env = "DATALITH_WORKERS", default_value = "1", global = true)]
    #[arg(value_parser = clap::value_parser!(u16).range(1..=64))]
    pub workers: u16,

    #[arg(long, env = "DATALITH_TASK_RETENTION_SECONDS", default_value = "604800", global = true)]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    pub task_retention_seconds: u64,

    #[arg(long, env = "DATALITH_PLAYBACK_SESSION_SECONDS", default_value = "86400", global = true)]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    pub playback_session_seconds: u64,

    #[arg(
        long,
        env = "DATALITH_MP4_EXPORT_RETENTION_SECONDS",
        default_value = "86400",
        global = true
    )]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    pub mp4_export_retention_seconds: u64,

    #[cfg(feature = "av-convert")]
    #[arg(long, env = "DATALITH_BITRATE", default_value = "12000k", global = true, value_parser = parse_bitrate)]
    #[arg(help = "1080p/60 fps bitrate limit in bits per second, with k, M, or G suffixes")]
    pub bitrate: u64,

    #[cfg(feature = "av-convert")]
    #[arg(long, env = "DATALITH_FFMPEG", default_value = "ffmpeg", global = true)]
    pub ffmpeg: PathBuf,

    #[cfg(feature = "av-convert")]
    #[arg(long, env = "DATALITH_FFPROBE", default_value = "ffprobe", global = true)]
    pub ffprobe: PathBuf,

    #[cfg(feature = "av-convert")]
    #[arg(long, env = "DATALITH_FFMPEG_PROCESSES", default_value = "1", global = true)]
    #[arg(value_parser = clap::value_parser!(u16).range(1..=64))]
    pub ffmpeg_processes: u16,

    #[cfg(feature = "av-convert")]
    #[arg(long, env = "DATALITH_FFMPEG_THREADS", global = true)]
    #[arg(value_parser = clap::value_parser!(u16).range(1..))]
    #[arg(help = "Encoder threads per process; defaults to half the available CPUs")]
    pub ffmpeg_threads: Option<u16>,

    #[cfg(feature = "image-convert")]
    #[arg(long, env = "DATALITH_MAX_IMAGE_RESOLUTION", default_value = "50000000", global = true)]
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub max_image_resolution: u32,

    #[cfg(feature = "image-convert")]
    #[arg(
        long,
        env = "DATALITH_MAX_IMAGE_RESOLUTION_MULTIPLIER",
        default_value = "3",
        global = true
    )]
    #[arg(value_parser = clap::value_parser!(u8).range(1..))]
    pub max_image_resolution_multiplier: u8,

    #[cfg(feature = "image-convert")]
    #[arg(long, env = "DATALITH_MAX_IMAGE_FRAMES", default_value = "500", global = true)]
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub max_image_frames: u32,

    #[cfg(feature = "image-convert")]
    #[arg(
        long,
        env = "DATALITH_MAX_IMAGE_TOTAL_PIXELS",
        default_value = "100000000",
        global = true
    )]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    pub max_image_total_pixels: u64,

    #[cfg(feature = "image-convert")]
    #[arg(long, env = "DATALITH_MAX_IMAGE_VARIANTS", default_value = "16", global = true)]
    #[arg(value_parser = clap::value_parser!(u16).range(1..))]
    pub max_image_variants: u16,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the HTTP service.
    Serve,
    /// Export all media or a selected list of media IDs.
    Export {
        #[arg(value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
        #[arg(long = "id")]
        ids:    Vec<Uuid>,
    },
    /// Import a Datalith archive into this environment.
    Import {
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
    },
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let seconds: u64 = value.parse().map_err(|_| "Expected a positive number of seconds.")?;
    if !(1..=36_000_000).contains(&seconds) {
        return Err("The lifespan must be between 1 and 36000000 seconds.".into());
    }
    Ok(Duration::from_secs(seconds))
}

pub fn get_args() -> CLIArgs {
    CLIArgs::parse()
}

#[cfg(feature = "av-convert")]
fn parse_bitrate(value: &str) -> Result<u64, String> {
    let value = value.trim();
    let (number, scale) =
        match value.as_bytes().last().copied().map(|byte| byte.to_ascii_lowercase()) {
            Some(b'k') => (&value[..value.len() - 1], 1000u64),
            Some(b'm') => (&value[..value.len() - 1], 1_000_000),
            Some(b'g') => (&value[..value.len() - 1], 1_000_000_000),
            _ => (value, 1),
        };
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(scale))
        .filter(|number| *number > 0)
        .ok_or_else(|| "Expected a positive bitrate, with an optional k, M, or G suffix.".into())
}
