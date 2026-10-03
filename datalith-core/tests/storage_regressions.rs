mod global;

use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};

use datalith_core::{PATH_TEMPORARY_FILE_DIRECTORY, PaginationOptions};
use global::*;
use tokio::{fs, io::AsyncWriteExt, task::JoinSet, time};

#[tokio::test]
async fn read_waiting_for_delete_does_not_keep_the_file_open() {
    let datalith = datalith_init().await;
    let file = datalith.put_file_by_buffer(b"content", Some("file.txt"), None).await.unwrap();
    let id = file.id();
    {
        let deleting = datalith.delete_file_by_id(id);
        let reading = datalith.get_file_by_id(id);
        tokio::pin!(deleting, reading);
        poll_fn(|cx| {
            assert!(deleting.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        poll_fn(|cx| {
            assert!(reading.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(file);
        let (deleted, read) =
            time::timeout(Duration::from_secs(2), async { tokio::join!(deleting, reading) })
                .await
                .unwrap();
        assert!(deleted.unwrap());
        assert!(read.unwrap().is_none());
    }
    datalith_close(datalith).await;
}

#[tokio::test]
async fn temporary_content_has_one_successful_claim() {
    let datalith = datalith_init().await;
    let file_id = datalith
        .put_file_by_buffer_temporarily(b"content", Some("file.txt"), None)
        .await
        .unwrap()
        .id();
    let resource_id = datalith
        .put_resource_by_buffer_temporarily(b"content", Some("resource.txt"), None)
        .await
        .unwrap()
        .id();
    let mut readers = JoinSet::new();
    for _ in 0..32 {
        let datalith = datalith.clone();
        readers.spawn(async move {
            let file = datalith.get_file_by_id(file_id).await.unwrap();
            let resource = datalith.get_resource_by_id(resource_id).await.unwrap();
            (usize::from(file.is_some()), usize::from(resource.is_some()))
        });
    }
    let mut file_claims = 0;
    let mut resource_claims = 0;
    while let Some(result) = readers.join_next().await {
        let (file, resource) = result.unwrap();
        file_claims += file;
        resource_claims += resource;
    }
    assert_eq!(1, file_claims);
    assert_eq!(1, resource_claims);
    datalith_close(datalith).await;
}

#[tokio::test]
async fn expiry_filters_pagination_and_releases_resource_references() {
    let datalith = datalith_init().await;
    datalith.set_temporary_file_lifespan(Duration::from_millis(100));
    for _ in 0..3 {
        datalith.put_file_by_buffer_temporarily(b"file", Some("file.txt"), None).await.unwrap();
        datalith
            .put_resource_by_buffer_temporarily(b"resource", Some("resource.txt"), None)
            .await
            .unwrap();
    }
    let permanent =
        datalith.put_resource_by_buffer(b"resource", Some("permanent.txt"), None).await.unwrap();
    let permanent_id = permanent.id();
    let file_id = permanent.file().id();
    drop(permanent);
    time::sleep(Duration::from_millis(120)).await;

    let (ids, pagination) = time::timeout(
        Duration::from_secs(2),
        datalith.list_file_ids(PaginationOptions::default().items_per_page(1).page(2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ids.is_empty());
    assert_eq!(1, pagination.get_total_items());
    let (ids, pagination) = time::timeout(
        Duration::from_secs(2),
        datalith.list_resource_ids(PaginationOptions::default().items_per_page(1).page(2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ids.is_empty());
    assert_eq!(1, pagination.get_total_items());
    assert_eq!(0, datalith.clear_untracked_files().await.unwrap());
    assert_eq!(6, datalith.clear_expired_files(Duration::from_secs(2)).await.unwrap());
    assert!(datalith.get_resource_by_id(permanent_id).await.unwrap().is_some());
    assert!(datalith.delete_resource_by_id(permanent_id).await.unwrap());
    assert!(!datalith.check_file_exist(file_id).await.unwrap());
    datalith_close(datalith).await;
}

#[tokio::test]
async fn cancelled_upload_removes_its_staging_file() {
    let datalith = datalith_init().await;
    let (mut writer, reader) = tokio::io::duplex(64);
    let uploading = {
        let datalith = datalith.clone();
        tokio::spawn(async move {
            datalith.put_file_by_reader(reader, Some("file.txt"), None, None).await
        })
    };
    writer.write_all(&[1; 1024]).await.unwrap();
    uploading.abort();
    assert!(uploading.await.unwrap_err().is_cancelled());
    drop(writer);
    let mut directory =
        fs::read_dir(datalith.get_environment().join(PATH_TEMPORARY_FILE_DIRECTORY)).await.unwrap();
    assert!(directory.next_entry().await.unwrap().is_none());
    datalith_close(datalith).await;
}
