use std::{
    net::{AddrParseError, IpAddr},
    num::ParseIntError,
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use byte_unit::Byte;
use clap::{CommandFactory, FromArgMatches, Parser};
use concat_with::concat_line;
use terminal_size::terminal_size;

const APP_NAME: &str = "Datalith";
const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const CARGO_PKG_AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

const APP_ABOUT: &str = concat!(
    "\nExamples:\n",
    concat_line!(prefix "datalith ",
        "                     # Start the service using the current working directory as the root of the environment",
        "--environment ./db   # Start the service using `./db` as the root of the environment",
    )
);

#[derive(Debug, Parser)]
#[command(name = APP_NAME)]
#[command(term_width = terminal_size().map(|(width, _)| width.0 as usize).unwrap_or(0))]
#[command(version = CARGO_PKG_VERSION)]
#[command(author = CARGO_PKG_AUTHORS)]
pub struct CLIArgs {
    #[arg(long, visible_alias = "addr", env = "DATALITH_ADDRESS")]
    #[cfg_attr(debug_assertions, arg(default_value = "127.0.0.1"))]
    #[cfg_attr(not(debug_assertions), arg(default_value = "0.0.0.0"))]
    #[arg(value_parser = parse_ip_addr)]
    #[arg(help = "Assign the address that Datalith binds")]
    pub address: IpAddr,

    #[arg(long, env = "DATALITH_LISTEN_PORT")]
    #[arg(default_value = "1111")]
    #[arg(help = "Assign a TCP port for the HTTP service")]
    pub listen_port: u16,

    #[arg(long, env = "DATALITH_ENVIRONMENT")]
    #[arg(value_hint = clap::ValueHint::DirPath)]
    #[arg(default_value = ".")]
    #[arg(help = "Assign the root path of the environment. This should be a directory path")]
    pub environment: PathBuf,

    #[arg(long, env = "DATALITH_MAX_FILE_SIZE")]
    #[arg(default_value = "2 GiB")]
    #[arg(help = "Assign the maximum file size (in bytes) for each of the uploaded files")]
    pub max_file_size: Byte,

    #[arg(long, env = "DATALITH_TEMPORARY_FILE_LIFESPAN")]
    #[arg(default_value = "60")]
    #[arg(help = "Assign the lifespan (in seconds) for each of the uploaded temporary files")]
    #[arg(long_help = "Assign the lifespan (in seconds) for each of the uploaded temporary \
                       files. The lifespan ranges from 1 second to 10,000 hours")]
    #[arg(value_parser = parse_duration_sec)]
    pub temporary_file_lifespan: Duration,

    #[cfg(feature = "image-convert")]
    #[arg(long, env = "DATALITH_MAX_IMAGE_RESOLUTION")]
    #[arg(default_value = "50000000")]
    #[arg(help = "Assign the maximum resolution (in pixels) for each of the uploaded images")]
    pub max_image_resolution: u32,

    #[cfg(feature = "image-convert")]
    #[arg(long, env = "DATALITH_MAX_IMAGE_RESOLUTION_MULTIPLIER")]
    #[arg(default_value = "3")]
    #[arg(help = "Assign the maximum image resolution multiplier for each of the uploaded images")]
    pub max_image_resolution_multiplier: u8,
}

#[inline]
fn parse_ip_addr(arg: &str) -> Result<IpAddr, AddrParseError> {
    IpAddr::from_str(arg)
}

#[inline]
fn parse_duration_sec(arg: &str) -> Result<Duration, ParseIntError> {
    Ok(Duration::from_secs(arg.parse()?))
}

pub fn get_args() -> CLIArgs {
    let args = CLIArgs::command();

    let about = format!("{APP_NAME} {CARGO_PKG_VERSION}\n{CARGO_PKG_AUTHORS}\n{APP_ABOUT}");

    let args = args.about(about);

    let matches = args.get_matches();

    match CLIArgs::from_arg_matches(&matches) {
        Ok(args) => args,
        Err(err) => {
            err.exit();
        },
    }
}
