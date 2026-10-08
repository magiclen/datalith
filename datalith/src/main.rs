#[macro_use]
extern crate rocket;

mod cli;
mod rocket_mounts;

use std::path::Path;

use cli::{Command, get_args};
use datalith_core::{
    Datalith, DatalithService, ExportOptions, PATH_TEMPORARY_FILE_DIRECTORY, ServiceConfig, Task,
    TaskStatus, Uuid,
};

// Run one queued task without processing the rest of the queue.
async fn run_task(service: &DatalithService, id: Uuid) -> anyhow::Result<Task> {
    let task = service.run_task(id).await?;
    match task.status {
        TaskStatus::Succeeded => Ok(task),
        TaskStatus::Failed => {
            let message = task
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "Unknown task error".into());
            anyhow::bail!("Task {id} failed: {message}");
        },
        TaskStatus::Cancelled => anyhow::bail!("Task {id} was cancelled."),
        _ => anyhow::bail!("Task {id} was interrupted."),
    }
}

async fn export(service: &DatalithService, output: &Path, ids: Vec<Uuid>) -> anyhow::Result<()> {
    // Create the output first, so that an existing file is reported before the export runs.
    let mut destination = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .await?
        .into_std()
        .await;
    let result = async {
        let options = ExportOptions {
            ids: if ids.is_empty() { None } else { Some(ids) }
        };
        let submitted = service.submit_export(options, None).await?;
        let task = run_task(service, submitted.id).await?;
        let content = service.open_artifact(task.id).await?;
        let mut source = content.file.into_std().await;
        // `std::io::copy` can copy between two files inside the kernel, and it avoids one blocking round trip for every small chunk.
        tokio::task::spawn_blocking(move || {
            std::io::copy(&mut source, &mut destination).and_then(|_| destination.sync_all())
        })
        .await??;
        anyhow::Ok(task)
    }
    .await;
    match result {
        Ok(task) => {
            println!("{}", serde_json::to_string(&task)?);
            Ok(())
        },
        Err(error) => {
            let _ = tokio::fs::remove_file(output).await;
            Err(error)
        },
    }
}

fn main() -> anyhow::Result<()> {
    let args = get_args();
    rocket::execute(async move {
        let datalith = Datalith::new(&args.environment).await?;
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
        let temporary_directory = datalith.get_environment().join(PATH_TEMPORARY_FILE_DIRECTORY);
        let command = args.command.unwrap_or(Command::Serve);
        // Transfers run only their own task, so that queued media work waits for the service.
        let service = if matches!(command, Command::Serve) {
            DatalithService::new(datalith, config).await?
        } else {
            DatalithService::new_without_workers(datalith, config).await?
        };
        let result: anyhow::Result<()> = match command {
            Command::Serve => {
                let rocket = rocket_mounts::create(
                    args.address,
                    args.listen_port,
                    args.max_file_size.as_u64(),
                    temporary_directory,
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
                    let submitted = service.submit_import_file(&file, None).await?;
                    let task = run_task(&service, submitted.id).await?;
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
