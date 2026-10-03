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
use sqlx::Row;
use uuid::Uuid;

use super::{
    DatalithService, ExportOptions, Media, MediaFile, PreparedFile, ServiceError,
    migration::content_path,
};
use crate::guard::OpenGuard;

const MAX_MANIFEST_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version:    u32,
    archive_id: Uuid,
    created_at: DateTime<Utc>,
    media:      Vec<Media>,
    files:      Vec<ArchiveFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveFile {
    sha256:    String,
    file_size: String,
}

struct ValidatedArchive {
    manifest:  Manifest,
    digest:    String,
    directory: tempfile::TempDir,
}

impl DatalithService {
    pub(super) async fn export_archive(
        &self,
        task_id: Uuid,
        options: ExportOptions,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, ServiceError> {
        let _gate = self.0.writes.write().await;
        check_cancelled(&cancel)?;
        let mutation = self.0.mutations.lock().await;
        let now = Utc::now();
        let rows = sqlx::query(
            "SELECT id, metadata FROM media WHERE (expires_at IS NULL OR expires_at > ?) AND \
             consumed_at IS NULL ORDER BY id",
        )
        .bind(now.timestamp_millis())
        .fetch_all(&self.0.datalith.0.db)
        .await?;
        let requested = options.ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());
        let mut media = Vec::new();
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            if requested.as_ref().is_none_or(|ids| ids.contains(&id)) {
                media.push(serde_json::from_str::<Media>(row.try_get("metadata")?)?);
            }
        }
        if requested.as_ref().is_some_and(|ids| ids.len() != media.len()) {
            return Err(ServiceError::NotFound);
        }
        let media_count = media.len();
        let mut contents = BTreeMap::<String, (u64, PathBuf)>::new();
        let mut guards = Vec::new();
        for item in &media {
            for file in media_files(item) {
                if contents.contains_key(&file.sha256) {
                    continue;
                }
                let size = parse_size(&file.file_size)?;
                guards.push(OpenGuard::new(self.0.datalith.clone(), file.id).await);
                let path = self.0.datalith.get_file_path(file.id).await?;
                contents.insert(file.sha256.clone(), (size, path));
            }
        }
        drop(mutation);
        let manifest = Manifest {
            version: 1,
            archive_id: task_id,
            created_at: now,
            media,
            files: contents
                .iter()
                .map(|(hash, (size, _))| ArchiveFile {
                    sha256:    hash.clone(),
                    file_size: size.to_string(),
                })
                .collect(),
        };
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
        let archive = tokio::task::spawn_blocking(move || {
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
            for file in media_files(media) {
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
        let now = Utc::now();
        for mut media in archive.manifest.media {
            check_cancelled(&cancel)?;
            let old_id = media.id;
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
                if equivalent_media(&existing, &media)? {
                    id_map.insert(old_id.to_string(), existing.id.to_string());
                    if let (Some(source), Some(target)) = (&media.original, &existing.original) {
                        let mapped = *resolved_files.entry(source.id).or_insert(target.id);
                        file_id_map.insert(source.id.to_string(), mapped.to_string());
                    }
                    for source in &media.variants {
                        if let Some(target) = existing.variants.iter().find(|target| {
                            target.name == source.name
                                && target.multiplier == source.multiplier
                                && target.format == source.format
                        }) {
                            let mapped =
                                *resolved_files.entry(source.file.id).or_insert(target.file.id);
                            file_id_map.insert(source.file.id.to_string(), mapped.to_string());
                        }
                    }
                    skipped += 1;
                    continue;
                }
                media.id = Uuid::new_v4();
            }
            let source_ids = media_files(&media).map(|file| file.id).collect::<Vec<_>>();
            for variant in &mut media.variants {
                variant.content_path =
                    content_path(media.id, &variant.name, variant.multiplier, &variant.format);
            }
            self.publish_import_media_tx(
                &mut tx,
                &mut media,
                &prepared,
                &mut guards,
                &mut resolved_files,
            )
            .await?;
            for (old, new) in source_ids.into_iter().zip(media_files(&media).map(|file| file.id)) {
                file_id_map.insert(old.to_string(), new.to_string());
            }
            id_map.insert(old_id.to_string(), media.id.to_string());
            imported += 1;
        }
        check_cancelled(&cancel)?;
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

fn media_files(media: &Media) -> impl Iterator<Item = &MediaFile> {
    media.original.iter().chain(media.variants.iter().map(|variant| &variant.file))
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
    #[cfg(unix)]
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
            if decoded.version != 1 {
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
    for media in &manifest.media {
        if !ids.insert(media.id) {
            return Err(archive_error("duplicate media ID"));
        }
        if media.original.is_none() && media.variants.is_empty() {
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
        for file in media_files(media) {
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
    Ok(())
}

fn equivalent_media(left: &Media, right: &Media) -> Result<bool, ServiceError> {
    fn normalized(media: &Media) -> Result<Value, ServiceError> {
        let mut media = media.clone();
        if let Some(file) = &mut media.original {
            file.id = Uuid::nil();
        }
        for variant in &mut media.variants {
            variant.file.id = Uuid::nil();
            variant.content_path.clear();
        }
        media.variants.sort_by(|left, right| {
            (&left.name, left.multiplier, &left.format).cmp(&(
                &right.name,
                right.multiplier,
                &right.format,
            ))
        });
        Ok(serde_json::to_value(media)?)
    }
    Ok(normalized(left)? == normalized(right)?)
}
