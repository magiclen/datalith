use std::{
    fmt,
    fmt::{Debug, Formatter},
    ops::Deref,
    time::Duration,
};

use chrono::Local;
use tokio_cron_scheduler::{Job, JobScheduler};

use crate::{Datalith, DatalithManagerError};

/// The Datalith file storage center manager.
#[derive(Clone)]
pub struct DatalithManager {
    datalith:  Datalith,
    scheduler: JobScheduler,
}

impl DatalithManager {
    pub async fn new(datalith: Datalith) -> Result<Self, DatalithManagerError> {
        let scheduler = JobScheduler::new().await?;

        {
            let datalith = datalith.clone();

            scheduler
                .add(Job::new_repeated_async(Duration::from_secs(60), move |_uuid, _l| {
                    let datalith = datalith.clone();

                    Box::pin(async move {
                        match datalith.clear_expired_files(Duration::from_secs(3)).await {
                            Ok(count) => match count {
                                0 => tracing::debug!("no expired file needs to be deleted"),
                                1 => tracing::info!("one expired file has been deleted"),
                                _ => tracing::info!("{count} expired files have been deleted"),
                            },
                            Err(error) => {
                                tracing::warn!("{error}");
                            },
                        }
                    })
                })?)
                .await?;
        }

        {
            let datalith = datalith.clone();

            scheduler
                .add(Job::new_async_tz("0 0 */4 * * *", Local, move |_uuid, _l| {
                    let datalith = datalith.clone();

                    Box::pin(async move {
                        match datalith.clear_untracked_files().await {
                            Ok(count) => match count {
                                0 => tracing::debug!("no untracked file needs to be deleted"),
                                1 => tracing::info!("one untracked file has been deleted"),
                                _ => tracing::info!("{count} untracked files haves been deleted"),
                            },
                            Err(error) => {
                                tracing::warn!("{error}");
                            },
                        }
                    })
                })?)
                .await?;
        }

        scheduler.start().await?;

        Ok(Self {
            datalith,
            scheduler,
        })
    }

    #[inline]
    pub async fn close(mut self) -> Result<(), DatalithManagerError> {
        self.scheduler.shutdown().await?;

        self.datalith.close().await;

        Ok(())
    }
}

impl Debug for DatalithManager {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.datalith, f)
    }
}

impl Deref for DatalithManager {
    type Target = Datalith;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.datalith
    }
}
