use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    io,
};

use mime::Mime;

/// Errors occurred during Datalith creation.
#[derive(Debug)]
pub enum DatalithCreateError {
    IOError(io::Error),
    SQLError(sqlx::Error),
    DatabaseTooNewError { app_db_version: u32, current_db_version: u32 },
    DatabaseTooOldError { app_db_version: u32, current_db_version: u32 },
    AlreadyRun,
}

impl From<io::Error> for DatalithCreateError {
    #[inline]
    fn from(error: io::Error) -> Self {
        Self::IOError(error)
    }
}

impl From<sqlx::Error> for DatalithCreateError {
    #[inline]
    fn from(error: sqlx::Error) -> Self {
        Self::SQLError(error)
    }
}

impl Display for DatalithCreateError {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IOError(error) => Display::fmt(error, f),
            Self::SQLError(error) => Display::fmt(error, f),
            Self::DatabaseTooNewError {
                app_db_version,
                current_db_version,
            } => f.write_fmt(format_args!(
                "this application is too old to use the database ({app_db_version} < \
                 {current_db_version})"
            )),
            Self::DatabaseTooOldError {
                app_db_version,
                current_db_version,
            } => f.write_fmt(format_args!(
                "this application is too new to upgrade the database ({app_db_version} > \
                 {current_db_version})"
            )),
            Self::AlreadyRun => f.write_str("there is already an existing instance"),
        }
    }
}

impl Error for DatalithCreateError {}

/// Errors occurred during Datalith read operations.
#[derive(Debug)]
pub enum DatalithReadError {
    IOError(io::Error),
    SQLError(sqlx::Error),
}

impl From<io::Error> for DatalithReadError {
    #[inline]
    fn from(error: io::Error) -> Self {
        Self::IOError(error)
    }
}

impl From<sqlx::Error> for DatalithReadError {
    #[inline]
    fn from(error: sqlx::Error) -> Self {
        Self::SQLError(error)
    }
}

impl Display for DatalithReadError {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::IOError(error) => Display::fmt(error, f),
            Self::SQLError(error) => Display::fmt(error, f),
        }
    }
}

impl Error for DatalithReadError {}

/// Errors occurred during Datalith write operations.
#[derive(Debug)]
pub enum DatalithWriteError {
    FileTypeInvalid { file_type: Mime, expected_file_type: Mime },
    FileLengthTooLarge { expected_file_length: u64, actual_file_length: u64 },
    IOError(io::Error),
    SQLError(sqlx::Error),
}

impl From<DatalithReadError> for DatalithWriteError {
    #[inline]
    fn from(error: DatalithReadError) -> Self {
        match error {
            DatalithReadError::IOError(error) => Self::IOError(error),
            DatalithReadError::SQLError(error) => Self::SQLError(error),
        }
    }
}

impl From<io::Error> for DatalithWriteError {
    #[inline]
    fn from(error: io::Error) -> Self {
        Self::IOError(error)
    }
}

impl From<sqlx::Error> for DatalithWriteError {
    #[inline]
    fn from(error: sqlx::Error) -> Self {
        Self::SQLError(error)
    }
}

impl Display for DatalithWriteError {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileTypeInvalid {
                file_type,
                expected_file_type,
            } => f.write_fmt(format_args!(
                "the file type {file_type:?} is invalid (expect: {expected_file_type:?})"
            )),
            Self::FileLengthTooLarge {
                expected_file_length,
                actual_file_length,
            } => f.write_fmt(format_args!(
                "the file length {actual_file_length:?} is larger than the expected one (expect: \
                 {expected_file_length:?})"
            )),
            Self::IOError(error) => Display::fmt(error, f),
            Self::SQLError(error) => Display::fmt(error, f),
        }
    }
}

impl Error for DatalithWriteError {}
