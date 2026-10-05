#[cfg(feature = "av-convert")]
use std::sync::atomic::Ordering;
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use chrono::Utc;
use tokio::fs;
use uuid::Uuid;

use super::{
    DatalithService, Mp4ExportOptions, Mp4Work, ServiceError, Task, Work, sessions::token_hash,
};
#[cfg(feature = "av-convert")]
use super::{
    Mp4ExportResult,
    hls::{target_duration, track_playlist},
    sessions::deadline,
    store::sync_directory,
};
#[cfg(feature = "av-convert")]
use crate::guard::OpenGuard;

impl DatalithService {
    pub(super) async fn expire_mp4_artifacts(&self) -> Result<(), ServiceError> {
        let Ok(_artifacts) = self.0.artifacts.try_write() else {
            return Ok(());
        };
        let _mutation = self.0.mutations.lock().await;
        let now = Utc::now().timestamp_millis();
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT task_id FROM mp4_artifacts WHERE expires_at<=? OR (session_hash IS NOT NULL \
             AND NOT EXISTS(SELECT 1 FROM playback_sessions s JOIN media m ON m.id=s.media_id \
             WHERE s.media_id=mp4_artifacts.media_id AND s.token_hash=mp4_artifacts.session_hash \
             AND s.expires_at>? AND (m.expires_at IS NULL OR m.expires_at>?)))",
        )
        .bind(now)
        .bind(now)
        .bind(now)
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        for id in ids {
            remove_if_present(&self.work_directory(id).join("export.mp4")).await?;
            sqlx::query("DELETE FROM mp4_artifacts WHERE task_id=?")
                .bind(id)
                .execute(&self.0.datalith.0.db)
                .await?;
        }
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE status IN ('failed','cancelled') AND \
             json_extract(metadata,'$.kind')='mp4_export' AND \
             ((json_extract(work,'$.Mp4Export.expires_at') IS NOT NULL AND \
             julianday(json_extract(work,'$.Mp4Export.expires_at'))<=julianday('now')) OR \
             (json_extract(work,'$.Mp4Export.session_hash') IS NOT NULL AND NOT EXISTS(SELECT 1 \
             FROM playback_sessions s JOIN media m ON m.id=s.media_id WHERE \
             s.token_hash=json_extract(tasks.work,'$.Mp4Export.session_hash') AND s.expires_at>? \
             AND (m.expires_at IS NULL OR m.expires_at>?))))",
        )
        .bind(now)
        .bind(now)
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        for id in ids {
            let path = self.work_directory(id).join("snapshot");
            match fs::remove_dir_all(path).await {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Snapshot and queue an MP4 export of an existing video variant.
    /// Single-use media requires its active playback credential.
    pub async fn submit_mp4_export(
        &self,
        media_id: Uuid,
        options: Mp4ExportOptions,
        session: Option<&str>,
        key: Option<String>,
    ) -> Result<Task, ServiceError> {
        super::tasks::validate_idempotency_key(key.as_deref())?;
        let identity = super::tasks::mp4_fingerprint(media_id, &options)?;
        if let Some(key) = &key
            && let Some(task) = self.find_idempotent_task(key, &identity).await?
        {
            let work = self.saved_mp4_work(task.id).await?;
            if work.session_hash.is_some() {
                self.verify_export_authorization(&work, session).await?;
            }
            return Ok(task);
        }
        #[cfg(not(feature = "av-convert"))]
        return Err(ServiceError::Unsupported("MP4 remuxing is disabled".into()));
        #[cfg(feature = "av-convert")]
        if !self.0.av.available {
            return Err(ServiceError::Unsupported(
                self.0.av.reason.clone().unwrap_or_else(|| "MP4 remuxing is unavailable".into()),
            ));
        }
        #[cfg(feature = "av-convert")]
        {
            let id = Uuid::new_v4();
            let directory = self.pending_directory(id).await?;
            let gate = self.0.writes.try_read().map_err(|_| ServiceError::Busy)?;
            let mutation = self.0.mutations.lock().await;
            let media = self.authorize_media(media_id, session).await?;
            let video = media.video.as_ref().ok_or(ServiceError::NotFound)?;
            let variant = video
                .variants
                .iter()
                .find(|variant| variant.id == options.variant)
                .ok_or(ServiceError::NotFound)?;
            let audio = ["flac", "aac_high", "aac_low"]
                .into_iter()
                .find(|id| variant.audio.iter().any(|candidate| candidate == id))
                .map(str::to_owned);
            let mut inventory = self.hls_inventory(media_id).await?;
            inventory
                .tracks
                .retain(|track| track.id == variant.id || audio.as_ref() == Some(&track.id));
            if inventory.tracks.len() != 1 + usize::from(audio.is_some()) {
                return Err(ServiceError::Internal("MP4 input tracks are missing".into()));
            }
            let hash = if media.single_use {
                Some(token_hash(session.ok_or(ServiceError::NotFound)?)?)
            } else {
                None
            };
            let mut expires_at = media.expires_at;
            if let Some(hash) = &hash {
                let expires: i64 = sqlx::query_scalar(
                    "SELECT expires_at FROM playback_sessions WHERE media_id=? AND token_hash=?",
                )
                .bind(media_id)
                .bind(hash)
                .fetch_one(&self.0.datalith.0.db)
                .await?;
                let expires = super::migration::timestamp(expires)?;
                expires_at = Some(expires_at.map_or(expires, |current| current.min(expires)));
            }
            let snapshot = directory.path.join("snapshot");
            fs::create_dir(&snapshot).await?;
            let mut guards = Vec::new();
            for track in &inventory.tracks {
                let path = snapshot.join(&track.id);
                fs::create_dir(&path).await?;
                let files =
                    std::iter::once((&track.initialization, "init.mp4".to_owned())).chain(
                        track.segments.iter().enumerate().map(|(index, segment)| {
                            (&segment.file, format!("segment-{index:06}.m4s"))
                        }),
                    );
                for (metadata, name) in files {
                    guards.push(OpenGuard::new(self.0.datalith.clone(), metadata.id).await);
                    let source = self.0.datalith.get_file_path(metadata.id).await?;
                    copy_snapshot(&source, &path.join(name)).await?;
                }
                sync_directory(&path).await?;
            }
            sync_directory(&snapshot).await?;
            sync_directory(&directory.path).await?;
            drop(guards);
            drop(mutation);
            drop(gate);
            let stem = std::path::Path::new(&media.file_name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("video");
            let work = Mp4Work {
                media_id,
                options,
                audio,
                inventory,
                file_name: format!("{stem}.mp4"),
                expires_at,
                session_hash: hash,
                retention_seconds: self.0.config.mp4_export_retention_seconds,
            };
            self.enqueue(id, Work::Mp4Export(work), key, directory).await
        }
    }

    pub(super) async fn saved_mp4_work(&self, id: Uuid) -> Result<Mp4Work, ServiceError> {
        let value: String = sqlx::query_scalar("SELECT work FROM tasks WHERE id=?")
            .bind(id)
            .fetch_optional(&self.0.datalith.0.db)
            .await?
            .ok_or(ServiceError::NotFound)?;
        match serde_json::from_str(&value)? {
            Work::Mp4Export(work) => Ok(work),
            _ => Err(ServiceError::NotFound),
        }
    }

    pub(super) async fn verify_export_authorization(
        &self,
        work: &Mp4Work,
        session: Option<&str>,
    ) -> Result<(), ServiceError> {
        if let Some(expected) = &work.session_hash {
            let hash = token_hash(session.ok_or(ServiceError::NotFound)?)?;
            if &hash != expected {
                return Err(ServiceError::NotFound);
            }
            self.authorize_media_hash(work.media_id, Some(&hash)).await?;
        }
        Ok(())
    }

    pub(super) async fn validate_export_work(&self, work: &Mp4Work) -> Result<(), ServiceError> {
        if work.expires_at.is_some_and(|expires| expires <= Utc::now()) {
            return Err(ServiceError::NotFound);
        }
        if let Some(hash) = &work.session_hash {
            self.authorize_media_hash(work.media_id, Some(hash)).await?;
        }
        Ok(())
    }

    #[cfg(not(feature = "av-convert"))]
    pub(super) async fn run_mp4_export(
        &self,
        _id: Uuid,
        _work: Mp4Work,
        _cancel: Arc<AtomicBool>,
    ) -> Result<serde_json::Value, ServiceError> {
        Err(ServiceError::Unsupported("MP4 remuxing is disabled".into()))
    }

    #[cfg(feature = "av-convert")]
    pub(super) async fn run_mp4_export(
        &self,
        id: Uuid,
        work: Mp4Work,
        cancel: Arc<AtomicBool>,
    ) -> Result<serde_json::Value, ServiceError> {
        use super::av_processor::executor::{ffmpeg_options, push};
        if !self.0.av.available {
            return Err(ServiceError::Unsupported(
                self.0.av.reason.clone().unwrap_or_else(|| "MP4 remuxing is unavailable".into()),
            ));
        }
        self.validate_export_work(&work).await?;
        let directory = self.work_directory(id);
        let snapshot = directory.join("snapshot");
        let target = target_duration(&work.inventory)?;
        for track in &work.inventory.tracks {
            fs::write(
                snapshot.join(&track.id).join("index.m3u8"),
                track_playlist(track, target, ""),
            )
            .await?;
        }
        let origin = work.inventory.presentation_start as f64 / f64::from(work.inventory.timescale);
        let output = directory.join(format!("mp4-{}.mp4", Uuid::new_v4()));
        let mut args = ffmpeg_options();
        push(&mut args, &["-copyts"]);
        for track in std::iter::once(work.options.variant.as_str()).chain(work.audio.as_deref()) {
            push(&mut args, &[
                "-itsoffset",
                &format!("{:.9}", -origin),
                "-protocol_whitelist",
                "file,crypto,data",
                "-format_whitelist",
                "hls,mov,mp4,m4a,3gp,3g2,mj2",
                "-allowed_extensions",
                "mp4,m4s",
                "-i",
            ]);
            args.push(snapshot.join(track).join("index.m3u8").into_os_string());
        }
        push(&mut args, &["-map", "0:v:0"]);
        if work.audio.is_some() {
            push(&mut args, &["-map", "1:a:0"]);
        }
        push(&mut args, &[
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-sn",
            "-dn",
            "-avoid_negative_ts",
            "disabled",
            "-movflags",
            "+faststart",
            "-strict",
            "experimental",
            "-f",
            "mp4",
        ]);
        args.push(output.as_os_str().into());
        let total = (work.inventory.duration as f64 / f64::from(work.inventory.timescale) * 1000.0)
            .ceil() as u64;
        self.processing_progress(id, "remuxing", 0, total).await?;
        self.0
            .av
            .run(
                &self.0.config.av.ffmpeg,
                &args,
                &cancel,
                Some((self, id, "remuxing", 0, total)),
                false,
                |_| Ok(()),
            )
            .await?;
        if cancel.load(Ordering::Acquire) {
            return Err(ServiceError::Cancelled);
        }
        let mut file =
            Self::prepare_file(output.clone(), "video/mp4".into(), work.file_name.clone()).await?;
        file.metadata.id = id;
        fs::rename(output, directory.join("export.mp4")).await?;
        sync_directory(&directory).await?;
        let _gate = self.0.writes.read().await;
        let _mutation = self.0.mutations.lock().await;
        self.validate_export_work(&work).await?;
        let expiry = deadline(work.retention_seconds)?;
        let expires_at = work.expires_at.map_or(expiry, |source_expiry| source_expiry.min(expiry));
        let expires_at = super::migration::timestamp(expires_at.timestamp_millis())?;
        let result = serde_json::to_value(Mp4ExportResult {
            media_id: work.media_id,
            variant: work.options.variant,
            audio: work.audio,
            artifact: file.metadata,
            artifact_path: format!("api/v1/tasks/{id}/artifact"),
            expires_at,
        })?;
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT OR REPLACE INTO mp4_artifacts(task_id,media_id,session_hash,expires_at) \
             VALUES(?,?,?,?)",
        )
        .bind(id)
        .bind(work.media_id)
        .bind(work.session_hash)
        .bind(expires_at.timestamp_millis())
        .execute(&mut *tx)
        .await?;
        Self::complete_task_tx(&mut tx, id, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
}

async fn remove_if_present(path: &Path) -> Result<(), ServiceError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(feature = "av-convert")]
async fn copy_snapshot(source: &Path, destination: &Path) -> Result<(), ServiceError> {
    if let Err(error) = fs::hard_link(source, destination).await {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        let mut source = fs::File::open(source).await?;
        let mut output =
            fs::OpenOptions::new().write(true).create_new(true).open(destination).await?;
        tokio::io::copy(&mut source, &mut output).await?;
        output.sync_all().await?;
    }
    Ok(())
}
