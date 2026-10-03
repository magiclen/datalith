use std::{path::Path, time::Duration};

use datalith_core::{
    ContentRequest, Datalith, DatalithService, Media, PATH_DB_FILE, ServiceConfig, ServiceError,
    Task, TaskStatus, UploadOptions, Uuid, chrono::Utc,
};
use sha2::{Digest, Sha256};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tempfile::TempDir;
use tokio::{fs, io::AsyncReadExt};

const CONTENT: &[u8] = b"A durable task keeps this upload across restarts.";

async fn wait_task(service: &DatalithService, id: Uuid) -> Task {
    tokio::time::timeout(Duration::from_secs(30), async {
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

async fn store_pending_task(
    directory: &Path,
    pool: &SqlitePool,
    status: TaskStatus,
    key: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let path = directory.join("datalith.tasks").join(id.to_string());
    fs::create_dir_all(&path).await.unwrap();
    fs::write(path.join("input"), CONTENT).await.unwrap();
    let options = UploadOptions::default();
    let hash = hex::encode(Sha256::digest(CONTENT));
    let work = format!(
        "{{\"Upload\":{{\"options\":{},\"hash\":\"{hash}\"}}}}",
        serde_json::to_string(&options).unwrap()
    );
    let now = Utc::now();
    let task = Task {
        id,
        kind: "resource".into(),
        status,
        stage: "storing".into(),
        completed_units: 0,
        total_units: Some(1),
        attempt: u32::from(status != TaskStatus::Queued),
        created_at: now,
        updated_at: now,
        result: None,
        error: None,
    };
    let state = match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        _ => unreachable!(),
    };
    sqlx::query(
        "INSERT INTO \
         tasks(id,status,created_at,updated_at,metadata,work,idempotency_key,fingerprint) \
         VALUES(?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(state)
    .bind(now.timestamp_millis())
    .bind(now.timestamp_millis())
    .bind(serde_json::to_string(&task).unwrap())
    .bind(&work)
    .bind(key)
    .bind(hex::encode(Sha256::digest(work.as_bytes())))
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn restart_recovers_work_without_duplicate_results() {
    let directory = TempDir::new().unwrap();
    Datalith::new(directory.path()).await.unwrap().close().await;
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(directory.path().join(PATH_DB_FILE)),
    )
    .await
    .unwrap();
    let queued =
        store_pending_task(directory.path(), &pool, TaskStatus::Queued, "queued-upload").await;
    let running =
        store_pending_task(directory.path(), &pool, TaskStatus::Running, "running-upload").await;
    let cancelling =
        store_pending_task(directory.path(), &pool, TaskStatus::Cancelling, "cancelled-upload")
            .await;
    pool.close().await;

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let queued_result = wait_task(&service, queued).await;
    let running_result = wait_task(&service, running).await;
    assert_eq!(1, queued_result.attempt);
    assert_eq!(2, running_result.attempt);
    assert_eq!(TaskStatus::Cancelled, service.get_task(cancelling).await.unwrap().unwrap().status);
    assert_eq!(TaskStatus::Queued, service.retry_task(cancelling).await.unwrap().status);
    assert_eq!(2, wait_task(&service, cancelling).await.attempt);

    let media: Media = serde_json::from_value(running_result.result.unwrap()).unwrap();
    let file_id = media.original.unwrap().id;
    let page = service.list_media(1, 100).await.unwrap();
    assert_eq!("3", page.total);
    assert!(page.items.iter().all(|item| item.original.as_ref().unwrap().id == file_id));
    service.close().await.unwrap();
    drop(service);

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let repeated = service
        .submit_upload(CONTENT, UploadOptions::default(), Some("running-upload".into()))
        .await
        .unwrap();
    assert_eq!(running, repeated.id);
    assert_eq!(TaskStatus::Succeeded, repeated.status);
    assert_eq!(2, repeated.attempt);
    assert_eq!("3", service.list_media(1, 100).await.unwrap().total);
    let different = service
        .submit_upload(
            &b"different content"[..],
            UploadOptions::default(),
            Some("running-upload".into()),
        )
        .await;
    assert!(matches!(different, Err(ServiceError::Conflict(_))));
    let mut content =
        service.open_content(media.id, ContentRequest::default(), false).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(CONTENT, bytes);
    drop(content);
    service.close().await.unwrap();
}
