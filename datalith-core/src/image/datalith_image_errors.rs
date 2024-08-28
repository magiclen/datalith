use std::{
    error::Error,
    fmt,
    fmt::{Display, Formatter},
    io,
};

use image_convert::MagickError;

use crate::{DatalithReadError, DatalithWriteError};

/// Errors occurred during Datalith image write operations.
#[derive(Debug)]
pub enum DatalithImageWriteError {
    DatalithWriteError(DatalithWriteError),
    UnsupportedImageType,
    ResolutionTooBig,
    MagickError(MagickError),
}

impl From<DatalithReadError> for DatalithImageWriteError {
    #[inline]
    fn from(error: DatalithReadError) -> Self {
        Self::DatalithWriteError(error.into())
    }
}

impl From<DatalithWriteError> for DatalithImageWriteError {
    #[inline]
    fn from(error: DatalithWriteError) -> Self {
        Self::DatalithWriteError(error)
    }
}

impl From<MagickError> for DatalithImageWriteError {
    #[inline]
    fn from(error: MagickError) -> Self {
        Self::MagickError(error)
    }
}

impl From<io::Error> for DatalithImageWriteError {
    #[inline]
    fn from(error: io::Error) -> Self {
        Self::DatalithWriteError(error.into())
    }
}

impl From<sqlx::Error> for DatalithImageWriteError {
    #[inline]
    fn from(error: sqlx::Error) -> Self {
        Self::DatalithWriteError(error.into())
    }
}

impl Display for DatalithImageWriteError {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatalithWriteError(error) => Display::fmt(&error, f),
            Self::UnsupportedImageType => f.write_str("supported image type"),
            Self::ResolutionTooBig => f.write_str("the image resolution is too big"),
            Self::MagickError(error) => Display::fmt(&error, f),
        }
    }
}

impl Error for DatalithImageWriteError {}
