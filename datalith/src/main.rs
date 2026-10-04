#[macro_use]
extern crate rocket;

mod cli;
mod rocket_mounts;

use std::{path::Path, time::Duration};

use cli::{Command, get_args};
use datalith_core::{
    Datalith, DatalithService, ExportOptions, ServiceConfig, Task, TaskStatus, Uuid,
};
use tokio::fs::{File, OpenOptions};

async fn wait_task(service: &DatalithService, id: Uuid) -> anyhow::Result<Task> {
    loop {
        let task = service
            .get_task(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task {id} was not found."))?;
        match task.status {
            TaskStatus::Succeeded => return Ok(task),
            TaskStatus::Failed => {
                let message = task
                    .error
                    .map(|error| error.message)
                    .unwrap_or_else(|| "Unknown task error".into());
                anyhow::bail!("Task {id} failed: {message}");
            },
            TaskStatus::Cancelled => anyhow::bail!("Task {id} was cancelled."),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn export(service: &DatalithService, output: &Path, ids: Vec<Uuid>) -> anyhow::Result<()> {
    let options = ExportOptions {
        ids: if ids.is_empty() { None } else { Some(ids) }
    };
    let submitted = service.submit_export(options, None).await?;
    let task = wait_task(service, submitted.id).await?;
    let mut content = service.open_artifact(task.id).await?;
    let mut destination = OpenOptions::new().write(true).create_new(true).open(output).await?;
    let result = async {
        tokio::io::copy(&mut content.file, &mut destination).await?;
        destination.sync_all().await
    }
    .await;
    drop(destination);
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(output).await;
        return Err(error.into());
    }
    println!("{}", serde_json::to_string(&task)?);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = get_args();
    rocket::execute(async move {
        let datalith = Datalith::new(&args.environment).await?;
        datalith.set_temporary_file_lifespan(args.temporary_file_lifespan);
        let config = ServiceConfig {
            max_file_size:                                       args.max_file_size.as_u64(),
            workers:                                             usize::from(args.workers),
            task_retention_seconds:                              args.task_retention_seconds,
            playback_session_seconds:                            args.playback_session_seconds,
            mp4_export_retention_seconds:                        args.mp4_export_retention_seconds,
            #[cfg(feature = "av-convert")]
            av:                                                  datalith_core::AvConfig {
                ffmpeg:          args.ffmpeg,
                ffprobe:         args.ffprobe,
                bitrate:         args.bitrate,
                max_processes:   usize::from(args.ffmpeg_processes),
                encoder_threads: args.ffmpeg_threads.map_or_else(
                    || datalith_core::AvConfig::default().encoder_threads,
                    usize::from,
                ),
            },
            #[cfg(not(feature = "av-convert"))]
            av:                                                  Default::default(),
            #[cfg(feature = "image-convert")]
            image_limits:                                        datalith_core::ImageLimits {
                max_pixels:       u64::from(args.max_image_resolution),
                max_frames:       args.max_image_frames,
                max_total_pixels: args.max_image_total_pixels,
                max_variants:     usize::from(args.max_image_variants),
                max_multiplier:   args.max_image_resolution_multiplier,
            },
            #[cfg(not(feature = "image-convert"))]
            image_limits:                                        Default::default(),
        };
        let service = DatalithService::new(datalith, config).await?;
        let result: anyhow::Result<()> = match args.command.unwrap_or(Command::Serve) {
            Command::Serve => {
                let rocket = rocket_mounts::create(
                    args.address,
                    args.listen_port,
                    args.max_file_size.as_u64(),
                )
                .manage(service.clone());
                rocket.launch().await.map(|_| ()).map_err(Into::into)
            },
            Command::Export {
                output,
                ids,
            } => export(&service, &output, ids).await,
            Command::Import {
                file,
            } => {
                async {
                    let reader = File::open(file).await?;
                    let submitted = service.submit_import(reader, None).await?;
                    let task = wait_task(&service, submitted.id).await?;
                    println!("{}", serde_json::to_string(&task)?);
                    Ok(())
                }
                .await
            },
        };
        let closed = service.close().await;
        result?;
        closed?;
        Ok(())
    })
}
