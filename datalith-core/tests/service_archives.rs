use std::{
    io::{Cursor, Read},
    time::Duration,
};

use datalith_core::{
    ContentRequest, Datalith, DatalithService, ExportOptions, Media, ServiceConfig, Task,
    TaskStatus, UploadOptions, Uuid,
};
use serde_json::Value;
use tokio::io::AsyncReadExt;

async fn service() -> (tempfile::TempDir, DatalithService) {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    (directory, service)
}

async fn completed(service: &DatalithService, id: Uuid) -> Task {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let task = service.get_task(id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn uploaded(service: &DatalithService, name: &str, content: &[u8]) -> Media {
    let task = service
        .submit_upload(
            content,
            UploadOptions {
                file_name: Some(name.into()),
                ..UploadOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    let task = completed(service, task.id).await;
    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
    serde_json::from_value(task.result.unwrap()).unwrap()
}

async fn exported(service: &DatalithService) -> Vec<u8> {
    let task = service.submit_export(ExportOptions::default(), None).await.unwrap();
    let task = completed(service, task.id).await;
    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
    let mut content = service.open_artifact(task.id).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    bytes
}

async fn imported(service: &DatalithService, bytes: &[u8]) -> Value {
    let task = service.submit_import(bytes, None).await.unwrap();
    let task = completed(service, task.id).await;
    assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
    task.result.unwrap()
}

fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    tar::Archive::new(Cursor::new(bytes))
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            (name, data)
        })
        .collect()
}

fn pack(entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o600);
        header.set_size(data.len() as u64);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        builder.append(&header, data.as_slice()).unwrap();
    }
    builder.into_inner().unwrap()
}

#[tokio::test]
async fn archive_roundtrip_merges_content_and_remaps_conflicting_media() {
    let (_source_directory, source) = service().await;
    let target_directory = tempfile::tempdir().unwrap();
    let target_storage = Datalith::new(target_directory.path()).await.unwrap();
    let target =
        DatalithService::new(target_storage.clone(), ServiceConfig::default()).await.unwrap();
    let payload = b"portable content";
    let first = uploaded(&source, "first.txt", payload).await;
    let second = uploaded(&source, "second.txt", payload).await;
    let existing = uploaded(&target, "existing.txt", payload).await;
    let bytes = exported(&source).await;
    assert_eq!(2, entries(&bytes).len());

    let result = imported(&target, &bytes).await;
    assert_eq!(2, result["imported"]);
    assert_eq!(0, result["skipped"]);
    assert_eq!(first.id.to_string(), result["id_map"][first.id.to_string()]);
    assert_eq!(second.id.to_string(), result["id_map"][second.id.to_string()]);
    assert_eq!("3", target.list_media(1, 100).await.unwrap().total);
    assert_eq!(
        first.original.as_ref().unwrap().id,
        target.get_media(first.id).await.unwrap().unwrap().original.unwrap().id
    );
    for id in [first.id, second.id, existing.id] {
        let mut content = target.open_content(id, ContentRequest::default(), false).await.unwrap();
        let mut actual = Vec::new();
        content.file.read_to_end(&mut actual).await.unwrap();
        assert_eq!(payload.as_slice(), actual);
    }
    let repeated = imported(&target, &bytes).await;
    assert_eq!(result, repeated);
    assert_eq!("3", target.list_media(1, 100).await.unwrap().total);

    let mut changed = entries(&bytes);
    let mut manifest: Value = serde_json::from_slice(&changed[0].1).unwrap();
    manifest["archive_id"] = serde_json::to_value(Uuid::new_v4()).unwrap();
    let source_media = manifest["media"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|media| media["id"] == first.id.to_string())
        .unwrap();
    source_media["file_name"] = Value::String("renamed.txt".into());
    changed[0].1 = serde_json::to_vec(&manifest).unwrap();
    let merged = imported(&target, &pack(changed)).await;
    assert_eq!(1, merged["imported"]);
    assert_eq!(1, merged["skipped"]);
    let remapped: Uuid = merged["id_map"][first.id.to_string()].as_str().unwrap().parse().unwrap();
    assert_ne!(first.id, remapped);
    assert_eq!("renamed.txt", target.get_media(remapped).await.unwrap().unwrap().file_name);
    assert_eq!("first.txt", target.get_media(first.id).await.unwrap().unwrap().file_name);
    assert_eq!("4", target.list_media(1, 100).await.unwrap().total);
    assert_eq!(
        1,
        std::fs::read_dir(target_directory.path().join(datalith_core::PATH_FILE_DIRECTORY))
            .unwrap()
            .count()
    );

    let database = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(target_directory.path().join(datalith_core::PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let mut content =
        target.open_content(first.id, ContentRequest::default(), false).await.unwrap();
    assert_ne!(existing.original.unwrap().id, content.metadata.id);
    for id in [first.id, second.id, existing.id, remapped] {
        assert!(target.delete_media(id).await.unwrap());
    }
    assert_eq!(0, target_storage.clear_untracked_files().await.unwrap());
    let mappings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM blob_files").fetch_one(&database).await.unwrap();
    assert_eq!(1, mappings);
    let mut actual = Vec::new();
    content.file.read_to_end(&mut actual).await.unwrap();
    assert_eq!(payload.as_slice(), actual);
    drop(content);
    source.close().await.unwrap();
    target.close().await.unwrap();
    drop(target);
    drop(target_storage);

    let restarted = DatalithService::new(
        Datalith::new(target_directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let mappings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM blob_files").fetch_one(&database).await.unwrap();
    assert_eq!(0, mappings);
    assert_eq!(
        0,
        std::fs::read_dir(target_directory.path().join(datalith_core::PATH_FILE_DIRECTORY))
            .unwrap()
            .count()
    );
    restarted.close().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn archive_rejects_traversal_and_changed_content_before_publishing() {
    let (_source_directory, source) = service().await;
    let (_target_directory, target) = service().await;
    uploaded(&source, "file.txt", b"portable content").await;
    let bytes = exported(&source).await;
    let mut traversal = entries(&bytes);
    traversal[1].0 = "../outside".into();
    let mut changed = entries(&bytes);
    changed[1].1[0] ^= 1;
    for invalid in [pack(traversal), pack(changed)] {
        let task = target.submit_import(invalid.as_slice(), None).await.unwrap();
        let task = completed(&target, task.id).await;
        assert_eq!(TaskStatus::Failed, task.status);
        assert_eq!("invalid_request", task.error.unwrap().code);
        assert_eq!("0", target.list_media(1, 100).await.unwrap().total);
    }
    source.close().await.unwrap();
    target.close().await.unwrap();
}

#[tokio::test]
async fn import_remaps_a_conflicting_file_id_consistently() {
    let (_source_directory, source) = service().await;
    let (_target_directory, target) = service().await;
    uploaded(&source, "first.txt", b"imported content").await;
    uploaded(&source, "second.txt", b"imported content").await;
    let existing = uploaded(&target, "existing.txt", b"existing content").await;
    let existing_file = existing.original.as_ref().unwrap().id;
    let mut packed = entries(&exported(&source).await);
    let mut manifest: Value = serde_json::from_slice(&packed[0].1).unwrap();
    for media in manifest["media"].as_array_mut().unwrap() {
        media["original"]["id"] = serde_json::to_value(existing_file).unwrap();
    }
    packed[0].1 = serde_json::to_vec(&manifest).unwrap();
    let result = imported(&target, &pack(packed)).await;
    assert_eq!(2, result["imported"]);
    let remapped: Uuid =
        result["file_id_map"][existing_file.to_string()].as_str().unwrap().parse().unwrap();
    assert_ne!(existing_file, remapped);
    for media in target.list_media(1, 100).await.unwrap().items {
        if media.id == existing.id {
            assert_eq!(existing_file, media.original.unwrap().id);
        } else {
            assert_eq!(remapped, media.original.unwrap().id);
        }
    }
    assert_eq!(
        2,
        std::fs::read_dir(_target_directory.path().join(datalith_core::PATH_FILE_DIRECTORY))
            .unwrap()
            .count()
    );
    source.close().await.unwrap();
    target.close().await.unwrap();
}
