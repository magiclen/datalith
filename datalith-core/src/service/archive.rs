use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};
use uuid::Uuid;

use super::{
    DatalithService, ExportOptions, HlsInventory, Media, MediaFile, PreparedFile, ServiceError,
    migration::content_path,
};
use crate::{PATH_FILE_DIRECTORY, guard::OpenGuard};

const MAX_MANIFEST_SIZE: u64 = 64 * 1024 * 1024;
const QUERY_BATCH_SIZE: usize = 400;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version:    u32,
    archive_id: Uuid,
    created_at: DateTime<Utc>,
    media:      Vec<Media>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    hls:        BTreeMap<Uuid, HlsInventory>,
    files:      Vec<ArchiveFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveFile {
    sha256:    String,
    file_size: String,
}

struct ArchiveSnapshot {
    manifest: Manifest,
    contents: BTreeMap<String, (u64, PathBuf)>,
    guards:   Vec<OpenGuard>,
}

struct ValidatedArchive {
    manifest:  Manifest,
    digest:    String,
    directory: tempfile::TempDir,
}

impl DatalithService {
    async fn snapshot_archive(
        &self,
        task_id: Uuid,
        options: ExportOptions,
        cancel: &AtomicBool,
    ) -> Result<ArchiveSnapshot, ServiceError> {
        let gate = self.0.writes.write().await;
        check_cancelled(cancel)?;
        let mutation = self.0.mutations.lock().await;
        let now = Utc::now();
        let requested = options.ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());
        let rows = if let Some(ids) = &requested {
            let ids: Vec<_> = ids.iter().copied().collect();
            let mut rows = Vec::new();
            for ids in ids.chunks(QUERY_BATCH_SIZE) {
                check_cancelled(cancel)?;
                let mut query = QueryBuilder::<Sqlite>::new(
                    "SELECT metadata FROM media WHERE consumed_at IS NULL AND (expires_at IS NULL \
                     OR expires_at > ",
                );
                query.push_bind(now.timestamp_millis()).push(") AND id IN (");
                let mut values = query.separated(",");
                for id in ids {
                    values.push_bind(*id);
                }
                values.push_unseparated(")");
                rows.extend(query.build().fetch_all(&self.0.datalith.0.db).await?);
            }
            rows
        } else {
            sqlx::query(
                "SELECT metadata FROM media WHERE (expires_at IS NULL OR expires_at > ?) AND \
                 consumed_at IS NULL",
            )
            .bind(now.timestamp_millis())
            .fetch_all(&self.0.datalith.0.db)
            .await?
        };
        if requested.as_ref().is_some_and(|ids| ids.len() != rows.len()) {
            return Err(ServiceError::NotFound);
        }
        let mut media = Vec::with_capacity(rows.len());
        for row in rows {
            media.push(serde_json::from_str::<Media>(row.try_get("metadata")?)?);
        }
        media.sort_unstable_by_key(|item| item.id);
        let mut hls = BTreeMap::new();
        for item in &media {
            if item.video.is_some() {
                hls.insert(item.id, self.hls_inventory(item.id).await?);
            }
        }
        let mut contents = BTreeMap::<String, (u64, PathBuf)>::new();
        let mut guards = Vec::new();
        let mut files = HashMap::new();
        let mut hashes = HashSet::new();
        for item in &media {
            for file in media_files(item, hls.get(&item.id)) {
                if !hashes.insert(file.sha256.clone()) {
                    continue;
                }
                let size = parse_size(&file.file_size)?;
                guards.push(OpenGuard::new(self.0.datalith.clone(), file.id).await);
                files.insert(file.id, (file.sha256.clone(), size));
            }
        }
        let ids: Vec<_> = files.keys().copied().collect();
        let file_directory = self.0.datalith.get_environment().join(PATH_FILE_DIRECTORY);
        for ids in ids.chunks(QUERY_BATCH_SIZE) {
            check_cancelled(cancel)?;
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT f.id, COALESCE(b.storage_id, f.id) FROM files f LEFT JOIN blob_files b ON \
                 b.file_id = f.id WHERE f.id IN (",
            );
            let mut values = query.separated(",");
            for id in ids {
                values.push_bind(*id);
            }
            values.push_unseparated(")");
            let paths: Vec<(Uuid, Uuid)> =
                query.build_query_as().fetch_all(&self.0.datalith.0.db).await?;
            if paths.len() != ids.len() {
                return Err(ServiceError::NotFound);
            }
            for (id, storage_id) in paths {
                let (hash, size) = files.remove(&id).ok_or(ServiceError::NotFound)?;
                contents.insert(
                    hash,
                    (size, file_directory.join(format!("{:x}", storage_id.as_u128()))),
                );
            }
        }
        drop(mutation);
        // The guards keep the selected files, so writes can continue while the archive is written from this snapshot.
        drop(gate);
        let manifest = Manifest {
            version: 2,
            archive_id: task_id,
            created_at: now,
            media,
            hls,
            files: contents
                .iter()
                .map(|(hash, (size, _))| ArchiveFile {
                    sha256:    hash.clone(),
                    file_size: size.to_string(),
                })
                .collect(),
        };
        Ok(ArchiveSnapshot {
            manifest,
            contents,
            guards,
        })
    }

    pub(super) async fn export_archive(
        &self,
        task_id: Uuid,
        options: ExportOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, ServiceError> {
        let ArchiveSnapshot {
            manifest,
            contents,
            guards,
        } = self.snapshot_archive(task_id, options, &cancel).await?;
        let media_count = manifest.media.len();
        let directory = self.work_directory(task_id);
        let worker_cancel = cancel.clone();
        let (sha256, file_size) = tokio::task::spawn_blocking(move || {
            write_archive(&directory, manifest, contents, &worker_cancel)
        })
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))??;
        drop(guards);
        let artifact = MediaFile {
            id: task_id,
            sha256,
            file_size: file_size.to_string(),
            file_type: "application/x-tar".into(),
            file_name: format!("datalith-{task_id}.tar"),
        };
        Ok(
            json!({"artifact_path": format!("api/v1/tasks/{task_id}/artifact"), "media_count": media_count, "artifact": artifact}),
        )
    }

    pub(super) async fn import_archive(
        &self,
        task_id: Uuid,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, ServiceError> {
        let directory = self.work_directory(task_id);
        let max_size = self.0.config.max_file_size;
        let worker_cancel = cancel.clone();
        let mut archive = tokio::task::spawn_blocking(move || {
            validate_archive(&directory, max_size, &worker_cancel)
        })
        .await
        .map_err(|error| ServiceError::Internal(error.to_string()))??;
        check_cancelled(&cancel)?;
        let _gate = self.0.writes.read().await;
        let _mutation = self.0.mutations.lock().await;
        check_cancelled(&cancel)?;
        let mut tx = self.0.datalith.0.db.begin_with("BEGIN IMMEDIATE").await?;
        let previous: Option<(String, String)> =
            sqlx::query_as("SELECT digest, result FROM archive_imports WHERE archive_id = ?")
                .bind(archive.manifest.archive_id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some((digest, result)) = previous {
            if digest != archive.digest {
                return Err(ServiceError::Conflict(
                    "archive ID was already used by different content".into(),
                ));
            }
            let result = serde_json::from_str(&result)?;
            Self::complete_task_tx(&mut tx, task_id, &result).await?;
            tx.commit().await?;
            return Ok(result);
        }
        let mut prepared = HashMap::new();
        for media in &archive.manifest.media {
            for file in media_files(media, archive.manifest.hls.get(&media.id)) {
                prepared.entry(file.id).or_insert_with(|| PreparedFile {
                    path:     archive.directory.path().join(&file.sha256),
                    metadata: file.clone(),
                });
            }
        }
        let mut id_map = BTreeMap::<String, String>::new();
        let mut file_id_map = BTreeMap::<String, String>::new();
        let mut resolved_files = HashMap::new();
        let mut imported = 0u64;
        let mut skipped = 0u64;
        let mut guards = Vec::new();
        let mut linked = false;
        let now = Utc::now();
        for mut media in archive.manifest.media {
            check_cancelled(&cancel)?;
            let old_id = media.id;
            let mut inventory = archive.manifest.hls.remove(&old_id);
            if media.consumed_at.is_some() || media.expires_at.is_some_and(|expiry| expiry <= now) {
                skipped += 1;
                continue;
            }
            let existing: Option<String> =
                sqlx::query_scalar("SELECT metadata FROM media WHERE id = ?")
                    .bind(media.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(existing) = existing {
                let existing: Media = serde_json::from_str(&existing)?;
                let existing_inventory: Option<String> =
                    sqlx::query_scalar("SELECT inventory FROM media_hls WHERE media_id=?")
                        .bind(existing.id)
                        .fetch_optional(&mut *tx)
                        .await?;
                let existing_inventory = existing_inventory
                    .map(|value| serde_json::from_str::<HlsInventory>(&value))
                    .transpose()?;
                if equivalent_media(
                    &existing,
                    &media,
                    existing_inventory.as_ref(),
                    inventory.as_ref(),
                )? {
                    id_map.insert(old_id.to_string(), existing.id.to_string());
                    let targets: HashMap<_, _> =
                        super::file_references(&existing, existing_inventory.as_ref())
                            .into_iter()
                            .collect();
                    for (role, source) in super::file_references(&media, inventory.as_ref()) {
                        let target = targets
                            .get(&role)
                            .ok_or_else(|| archive_error("equivalent media has different roles"))?;
                        let mapped = *resolved_files.entry(source.id).or_insert(target.id);
                        file_id_map.insert(source.id.to_string(), mapped.to_string());
                    }
                    skipped += 1;
                    continue;
                }
                media.id = Uuid::new_v4();
            }
            let source_ids =
                media_files(&media, inventory.as_ref()).map(|file| file.id).collect::<Vec<_>>();
            refresh_paths(&mut media);
            linked |= self
                .publish_import_media_tx(
                    &mut tx,
                    &mut media,
                    inventory.as_mut(),
                    &prepared,
                    &mut guards,
                    &mut resolved_files,
                )
                .await?;
            for (old, new) in source_ids
                .into_iter()
                .zip(media_files(&media, inventory.as_ref()).map(|file| file.id))
            {
                file_id_map.insert(old.to_string(), new.to_string());
            }
            id_map.insert(old_id.to_string(), media.id.to_string());
            imported += 1;
        }
        check_cancelled(&cancel)?;
        if linked {
            self.sync_file_directory().await?;
        }
        let result = json!({"archive_id": archive.manifest.archive_id, "imported": imported, "skipped": skipped, "id_map": id_map, "file_id_map": file_id_map});
        sqlx::query("INSERT INTO archive_imports(archive_id, digest, result) VALUES(?,?,?)")
            .bind(archive.manifest.archive_id)
            .bind(&archive.digest)
            .bind(serde_json::to_string(&result)?)
            .execute(&mut *tx)
            .await?;
        Self::complete_task_tx(&mut tx, task_id, &result).await?;
        tx.commit().await?;
        drop(guards);
        Ok(result)
    }
}

fn media_files<'a>(
    media: &'a Media,
    inventory: Option<&'a HlsInventory>,
) -> impl Iterator<Item = &'a MediaFile> {
    super::file_references(media, inventory).into_iter().map(|(_, file)| file)
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), ServiceError> {
    if cancel.load(Ordering::Acquire) { Err(ServiceError::Cancelled) } else { Ok(()) }
}

fn parse_size(size: &str) -> Result<u64, ServiceError> {
    size.parse::<u64>()
        .ok()
        .filter(|size| *size <= i64::MAX as u64)
        .ok_or_else(|| ServiceError::Invalid("invalid archive file size".into()))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn archive_error(message: &str) -> ServiceError {
    ServiceError::Invalid(message.into())
}

fn append_header<W: Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    reader: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, path, reader)
}

fn write_archive(
    directory: &Path,
    manifest: Manifest,
    contents: BTreeMap<String, (u64, PathBuf)>,
    cancel: &Arc<AtomicBool>,
) -> Result<(String, u64), ServiceError> {
    check_cancelled(cancel)?;
    let manifest = serde_json::to_vec(&manifest)?;
    if manifest.len() as u64 > MAX_MANIFEST_SIZE {
        return Err(archive_error("archive manifest exceeds 64 MiB; export smaller groups"));
    }
    let mut output = tempfile::NamedTempFile::new_in(directory)?;
    let mut writer = HashingWriter {
        inner: output.as_file_mut(), hash: Sha256::new(), size: 0
    };
    {
        let mut builder = tar::Builder::new(&mut writer);
        append_header(&mut builder, "manifest.json", manifest.len() as u64, manifest.as_slice())?;
        for (hash, (size, path)) in contents {
            check_cancelled(cancel)?;
            let source = File::open(path)?;
            if source.metadata()?.len() != size {
                return Err(archive_error("stored file size does not match metadata"));
            }
            let mut source =
                HashingReader {
                    inner: source, hash: Sha256::new(), cancel: cancel.clone()
                };
            if let Err(error) =
                append_header(&mut builder, &format!("blobs/{hash}"), size, &mut source)
            {
                check_cancelled(cancel)?;
                return Err(error.into());
            }
            if hex::encode(source.hash.finalize()) != hash {
                return Err(archive_error("stored file hash does not match metadata"));
            }
        }
        builder.finish()?;
    }
    writer.flush()?;
    let digest = hex::encode(writer.hash.finalize());
    let size = writer.size;
    output.as_file().sync_all()?;
    check_cancelled(cancel)?;
    output.persist(directory.join("export.tar")).map_err(|error| error.error)?;
    File::open(directory)?.sync_all()?;
    Ok((digest, size))
}

struct HashingWriter<W> {
    inner: W,
    hash:  Sha256,
    size:  u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.hash.update(&buffer[..count]);
        self.size += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct HashingReader<R> {
    inner:  R,
    hash:   Sha256,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(io::Error::other("task cancelled"));
        }
        let count = self.inner.read(buffer)?;
        self.hash.update(&buffer[..count]);
        Ok(count)
    }
}

fn validate_archive(
    directory: &Path,
    max_size: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<ValidatedArchive, ServiceError> {
    let result = validate_archive_inner(directory, max_size, cancel);
    check_cancelled(cancel)?;
    result
}

fn validate_archive_inner(
    directory: &Path,
    max_size: u64,
    cancel: &Arc<AtomicBool>,
) -> Result<ValidatedArchive, ServiceError> {
    let input = File::open(directory.join("input"))?;
    if input.metadata()?.len() > max_size {
        return Err(ServiceError::PayloadTooLarge);
    }
    let reader = HashingReader {
        inner: input, hash: Sha256::new(), cancel: cancel.clone()
    };
    let mut archive = tar::Archive::new(reader);
    let extracted = tempfile::Builder::new().prefix("import-").tempdir_in(directory)?;
    let mut manifest: Option<Manifest> = None;
    let mut declared = HashMap::<String, u64>::new();
    let mut seen = HashSet::new();
    let mut extracted_size = 0u64;
    for entry in archive.entries()?.raw(true) {
        check_cancelled(cancel)?;
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err(archive_error("archive entries must be regular files"));
        }
        let path = entry.path_bytes();
        let path = std::str::from_utf8(&path)
            .map_err(|_| archive_error("invalid archive path"))?
            .to_owned();
        if !seen.insert(path.clone()) {
            return Err(archive_error("duplicate archive entry"));
        }
        let size = entry.size();
        extracted_size = extracted_size.checked_add(size).ok_or(ServiceError::PayloadTooLarge)?;
        if extracted_size > max_size {
            return Err(ServiceError::PayloadTooLarge);
        }
        if path == "manifest.json" {
            if manifest.is_some() || seen.len() != 1 || size > MAX_MANIFEST_SIZE {
                return Err(archive_error("invalid archive manifest"));
            }
            let decoded: Manifest = serde_json::from_reader(&mut entry)
                .map_err(|_| archive_error("invalid archive manifest JSON"))?;
            if !matches!(decoded.version, 1 | 2) {
                return Err(ServiceError::Unsupported("unsupported archive version".into()));
            }
            for file in &decoded.files {
                if !valid_hash(&file.sha256)
                    || declared.insert(file.sha256.clone(), parse_size(&file.file_size)?).is_some()
                {
                    return Err(archive_error("invalid or duplicate archive file hash"));
                }
            }
            validate_manifest(&decoded, &declared)?;
            manifest = Some(decoded);
            continue;
        }
        if manifest.is_none() {
            return Err(archive_error("manifest.json must be the first entry"));
        }
        let hash = path
            .strip_prefix("blobs/")
            .filter(|hash| valid_hash(hash))
            .ok_or_else(|| archive_error("invalid archive path"))?;
        if declared.get(hash) != Some(&size) {
            return Err(archive_error("archive entry does not match its manifest"));
        }
        let mut output =
            File::options().write(true).create_new(true).open(extracted.path().join(hash))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancelled(cancel)?;
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
        if hex::encode(hasher.finalize()) != hash {
            return Err(archive_error("archive content hash does not match"));
        }
    }
    let manifest = manifest.ok_or_else(|| archive_error("missing archive manifest"))?;
    if seen.len() != declared.len() + 1 {
        return Err(archive_error("missing archive file content"));
    }
    let mut reader = archive.into_inner();
    io::copy(&mut reader, &mut io::sink())?;
    Ok(ValidatedArchive {
        manifest,
        digest: hex::encode(reader.hash.finalize()),
        directory: extracted,
    })
}

fn validate_manifest(
    manifest: &Manifest,
    declared: &HashMap<String, u64>,
) -> Result<(), ServiceError> {
    let mut ids = HashSet::new();
    let mut referenced = HashSet::new();
    let mut file_ids = HashMap::<Uuid, (&str, u64)>::new();
    if manifest.version == 1 && !manifest.hls.is_empty() {
        return Err(archive_error("version 1 archives cannot contain HLS inventories"));
    }
    for media in &manifest.media {
        let inventory = manifest.hls.get(&media.id);
        super::validate_assets(media, inventory)?;
        if manifest.version == 1
            && (media.audio.is_some()
                || media.video.is_some()
                || matches!(media.kind, super::MediaKind::Audio | super::MediaKind::Video))
        {
            return Err(archive_error("version 1 archives cannot contain audio or video outputs"));
        }
        if !ids.insert(media.id) {
            return Err(archive_error("duplicate media ID"));
        }
        if media_files(media, inventory).next().is_none() {
            return Err(archive_error("media has no files"));
        }
        if media.file_name.chars().any(char::is_control) {
            return Err(archive_error("invalid media file name"));
        }
        if media.kind == super::MediaKind::Resource
            && (media.original.is_none() || !media.variants.is_empty())
        {
            return Err(archive_error("resources must have one original file"));
        }
        let mut roles = HashSet::new();
        for variant in &media.variants {
            let name = &variant.name;
            let format = &variant.format;
            if name.is_empty()
                || name == "original"
                || name.len() > 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || format.is_empty()
                || !format.bytes().all(|byte| byte.is_ascii_alphanumeric())
                || variant.multiplier == 0
                || variant.width == 0
                || variant.height == 0
                || !roles.insert((name, variant.multiplier, format))
            {
                return Err(archive_error("invalid or duplicate media variant"));
            }
        }
        for file in media_files(media, inventory) {
            let size = parse_size(&file.file_size)?;
            if file.file_name.chars().any(char::is_control)
                || file.file_type.parse::<mime::Mime>().is_err()
            {
                return Err(archive_error("invalid file name or MIME type"));
            }
            if declared.get(&file.sha256) != Some(&size) {
                return Err(archive_error("media references invalid file content"));
            }
            if file_ids
                .insert(file.id, (&file.sha256, size))
                .is_some_and(|value| value != (&file.sha256, size))
            {
                return Err(archive_error("one file ID references different content"));
            }
            referenced.insert(file.sha256.as_str());
        }
    }
    if referenced.len() != declared.len() {
        return Err(archive_error("archive contains unreferenced file content"));
    }
    if manifest.hls.keys().any(|id| !ids.contains(id)) {
        return Err(archive_error("HLS inventory has no matching media"));
    }
    Ok(())
}

fn equivalent_media(
    left: &Media,
    right: &Media,
    left_inventory: Option<&HlsInventory>,
    right_inventory: Option<&HlsInventory>,
) -> Result<bool, ServiceError> {
    fn normalized(media: &Media, inventory: Option<&HlsInventory>) -> Result<Value, ServiceError> {
        let mut media = media.clone();
        let mut inventory = inventory.cloned();
        for file in super::files_mut(&mut media, inventory.as_mut()) {
            file.id = Uuid::nil();
        }
        for variant in &mut media.variants {
            variant.file.id = Uuid::nil();
            variant.content_path.clear();
        }
        if let Some(audio) = &mut media.audio {
            for variant in &mut audio.variants {
                variant.content_path.clear();
            }
        }
        if let Some(video) = &mut media.video {
            video.master_path.clear();
            for variant in &mut video.variants {
                variant.playlist_path.clear();
            }
            for audio in &mut video.audio {
                audio.content_path.clear();
            }
        }
        media.variants.sort_by(|left, right| {
            (&left.name, left.multiplier, &left.format).cmp(&(
                &right.name,
                right.multiplier,
                &right.format,
            ))
        });
        Ok(serde_json::json!({"media":media,"hls":inventory}))
    }
    Ok(normalized(left, left_inventory)? == normalized(right, right_inventory)?)
}

fn refresh_paths(media: &mut Media) {
    for variant in &mut media.variants {
        variant.content_path =
            content_path(media.id, &variant.name, variant.multiplier, &variant.format);
    }
    if let Some(audio) = &mut media.audio {
        for variant in &mut audio.variants {
            variant.content_path = format!(
                "api/v1/media/{}/content?format={}",
                media.id,
                if variant.codec == "flac" { "flac" } else { "m4a" }
            );
        }
    }
    if let Some(video) = &mut media.video {
        video.master_path = format!("api/v1/media/{}/hls/master.m3u8", media.id);
        for variant in &mut video.variants {
            variant.playlist_path =
                format!("api/v1/media/{}/hls/{}/index.m3u8", media.id, variant.id);
        }
        for audio in &mut video.audio {
            audio.content_path = format!("api/v1/media/{}/hls/{}/index.m3u8", media.id, audio.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{fs, io::AsyncReadExt};

    use super::*;
    use crate::{ContentRequest, Datalith, ServiceConfig, Task, TaskStatus, UploadOptions};

    async fn finished(service: &DatalithService, id: Uuid) -> Task {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let task = service.get_task(id).await.unwrap().unwrap();
                if task.status.is_terminal() {
                    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn export_snapshot_keeps_aliases_readable_after_deletion_and_cleanup() {
        let source_directory = tempfile::tempdir().unwrap();
        let source = DatalithService::new(
            Datalith::new(source_directory.path()).await.unwrap(),
            ServiceConfig::default(),
        )
        .await
        .unwrap();
        let payload = b"Snapshot content stays readable.";
        let mut ids = Vec::new();
        for name in ["first.txt", "second.txt"] {
            let task = source
                .submit_upload(
                    payload.as_slice(),
                    UploadOptions {
                        file_name: Some(name.into()),
                        ..UploadOptions::default()
                    },
                    None,
                )
                .await
                .unwrap();
            let media: Media =
                serde_json::from_value(finished(&source, task.id).await.result.unwrap()).unwrap();
            ids.push(media.id);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let ArchiveSnapshot {
            manifest,
            contents,
            guards,
        } = source
            .snapshot_archive(Uuid::new_v4(), ExportOptions::default(), &cancel)
            .await
            .unwrap();
        assert_eq!(2, manifest.media.len());
        assert_eq!(1, contents.len());
        let stored_path = contents.values().next().unwrap().1.clone();

        // The snapshot has released the write gate, while its guard still protects the shared content.
        for id in &ids {
            assert!(source.delete_media(*id).await.unwrap());
        }
        source.clear_released_files().await.unwrap();
        assert!(source.clear_untracked_files().await.unwrap());
        assert!(fs::try_exists(&stored_path).await.unwrap());
        let mappings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob_files")
            .fetch_one(&source.0.datalith.0.db)
            .await
            .unwrap();
        assert_eq!(1, mappings);

        let archive_directory = tempfile::tempdir().unwrap();
        write_archive(archive_directory.path(), manifest, contents, &cancel).unwrap();
        drop(guards);
        source.clear_released_files().await.unwrap();
        source.clear_untracked_files().await.unwrap();
        assert!(!fs::try_exists(&stored_path).await.unwrap());

        let target_directory = tempfile::tempdir().unwrap();
        let target = DatalithService::new(
            Datalith::new(target_directory.path()).await.unwrap(),
            ServiceConfig::default(),
        )
        .await
        .unwrap();
        let task = target
            .submit_import_file(archive_directory.path().join("export.tar"), None)
            .await
            .unwrap();
        let result = finished(&target, task.id).await.result.unwrap();
        assert_eq!(2, result["imported"]);
        for id in ids {
            let mut content =
                target.open_content(id, ContentRequest::default(), false).await.unwrap();
            let mut actual = Vec::new();
            content.file.read_to_end(&mut actual).await.unwrap();
            assert_eq!(payload.as_slice(), actual);
            assert_eq!(hex::encode(Sha256::digest(payload)), content.metadata.sha256);
        }
        source.close().await.unwrap();
        target.close().await.unwrap();
    }
}
