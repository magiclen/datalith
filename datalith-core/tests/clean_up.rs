use std::time::Duration;

use datalith_core::{
    ContentRequest, Datalith, DatalithService, Media, PATH_FILE_DIRECTORY, Retention,
    ServiceConfig, TaskStatus, UploadOptions,
};
use tokio::{fs, io::AsyncReadExt, time};

async fn upload(service: &DatalithService, single_use: bool) -> Media {
    let task = service
        .submit_upload(
            b"shared content".as_slice(),
            UploadOptions {
                retention: Retention {
                    single_use,
                    ..Retention::default()
                },
                ..UploadOptions::default()
            },
            None,
        )
        .await
        .unwrap();
    time::timeout(Duration::from_secs(10), async {
        loop {
            let task = service.get_task(task.id).await.unwrap().unwrap();
            if task.status.is_terminal() {
                assert_eq!(TaskStatus::Succeeded, task.status, "{:?}", task.error);
                break serde_json::from_value(task.result.unwrap()).unwrap();
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn startup_collects_consumed_media_and_orphans_without_removing_shared_content() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let permanent = upload(&service, false).await;
    let temporary = upload(&service, true).await;
    assert_eq!(permanent.original.as_ref().unwrap().id, temporary.original.unwrap().id);
    let content =
        service.open_content(temporary.id, ContentRequest::default(), false).await.unwrap();
    drop(content);
    assert!(service.get_media(temporary.id).await.unwrap().is_none());
    service.close().await.unwrap();
    drop(service);

    let files = directory.path().join(PATH_FILE_DIRECTORY);
    let unexpected_name = files.join("orphan.txt");
    let unexpected_id = files.join("70b7c850506e4fa98a4a713aca21f594");
    let unexpected_directory = files.join("orphan-directory");
    fs::write(&unexpected_name, b"orphan").await.unwrap();
    fs::write(&unexpected_id, b"orphan").await.unwrap();
    fs::create_dir(&unexpected_directory).await.unwrap();
    fs::write(unexpected_directory.join("content"), b"orphan").await.unwrap();

    let service = DatalithService::new(
        Datalith::new(directory.path()).await.unwrap(),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!("1", service.list_media(1, 100).await.unwrap().total);
    for path in [unexpected_name, unexpected_id, unexpected_directory] {
        assert!(!fs::try_exists(path).await.unwrap());
    }
    let mut content =
        service.open_content(permanent.id, ContentRequest::default(), false).await.unwrap();
    let mut bytes = Vec::new();
    content.file.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(b"shared content".as_slice(), bytes);
    drop(content);
    service.close().await.unwrap();
}
