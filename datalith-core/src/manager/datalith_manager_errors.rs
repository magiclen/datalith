use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

use tokio_cron_scheduler::JobSchedulerError;

/// Errors occurred during `DatalithManager` creation.
#[derive(Debug)]
pub enum DatalithManagerError {
    JobSchedulerError(JobSchedulerError),
}

impl From<JobSchedulerError> for DatalithManagerError {
    #[inline]
    fn from(error: JobSchedulerError) -> Self {
        Self::JobSchedulerError(error)
    }
}

impl Display for DatalithManagerError {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::JobSchedulerError(error) => Display::fmt(error, f),
        }
    }
}

impl Error for DatalithManagerError {}
