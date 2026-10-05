use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    io,
};

/// Errors when creating a Datalith store.
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

/// Errors when reading from Datalith.
#[derive(Debug)]
pub(crate) enum DatalithReadError {
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
