mod global;

use std::time::Duration;

use datalith_core::PaginationOptions;
use global::*;
use tokio::time;

#[tokio::test]
async fn parallel_and_temporary() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let (file_1, file_2, file_3, file_4) = tokio::join!(
            datalith.put_file_by_buffer_temporarily(image, Some("image.png"), None),
            datalith.put_file_by_buffer_temporarily(image, Some("image.png"), None),
            datalith.put_file_by_buffer(image, Some("image.png"), None),
            datalith.put_file_by_buffer(image, Some("image.png"), None),
        );

        let file_1 = file_1.unwrap();
        let file_2 = file_2.unwrap();
        let file_3 = file_3.unwrap();
        let file_4 = file_4.unwrap();

        assert_ne!(file_1, file_2);
        assert_ne!(file_1, file_3);
        assert_ne!(file_2, file_3);
        assert_eq!(file_3, file_4);

        let (file_ids, _) = datalith.list_file_ids(PaginationOptions::default()).await.unwrap();
        assert_eq!(3, file_ids.len());

        let (id_1, id_2, id_3_4) = (file_1.id(), file_2.id(), file_3.id());

        let (delete_result_1, delete_result_2, delete_result_3, delete_result_4) = tokio::join!(
            time::timeout(Duration::from_secs(1), datalith.delete_file_by_id(id_1)),
            time::timeout(Duration::from_secs(1), datalith.delete_file_by_id(id_2)),
            time::timeout(Duration::from_secs(1), datalith.delete_file_by_id(id_3_4)),
            time::timeout(Duration::from_secs(1), datalith.delete_file_by_id(id_3_4)),
        );

        // timeout errors will be thrown
        assert!(delete_result_1.is_err());
        assert!(delete_result_2.is_err());
        // 3 or 4 will be deleted successfully because they are the same file and the **count** is 2. After deleting, the count will be updated to 1
        assert!(delete_result_3.is_err() ^ delete_result_4.is_err());

        drop(file_1);
        drop(file_2);
        drop(file_3);
        drop(file_4);

        let (delete_result_1, delete_result_2, delete_result_3_4) = tokio::join!(
            datalith.delete_file_by_id(id_1),
            datalith.delete_file_by_id(id_2),
            datalith.delete_file_by_id(id_3_4),
        );

        assert!(delete_result_1.unwrap());
        assert!(delete_result_2.unwrap());
        assert!(delete_result_3_4.unwrap());

        let (delete_result_1, delete_result_2, delete_result_3_4) = tokio::join!(
            datalith.delete_file_by_id(id_1),
            datalith.delete_file_by_id(id_2),
            datalith.delete_file_by_id(id_3_4),
        );

        assert!(!delete_result_1.unwrap());
        assert!(!delete_result_2.unwrap());
        assert!(!delete_result_3_4.unwrap());
    }

    datalith_close(datalith).await;
}

#[tokio::test]
async fn resource_parallel_and_temporary() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let (resource_1, resource_2, resource_3, resource_4) = tokio::join!(
            datalith.put_resource_by_buffer_temporarily(image, Some("image.png"), None),
            datalith.put_resource_by_buffer_temporarily(image, Some("image.png"), None),
            datalith.put_resource_by_buffer(image, Some("image.png"), None),
            datalith.put_resource_by_buffer(image, Some("image.png"), None),
        );

        let resource_1 = resource_1.unwrap();
        let resource_2 = resource_2.unwrap();
        let resource_3 = resource_3.unwrap();
        let resource_4 = resource_4.unwrap();

        assert_ne!(resource_1, resource_2);
        assert_ne!(resource_1, resource_3);
        assert_ne!(resource_2, resource_3);
        assert_ne!(resource_3, resource_4);

        let (file_ids, _) = datalith.list_resource_ids(PaginationOptions::default()).await.unwrap();
        assert_eq!(4, file_ids.len());

        let (id_1, id_2, id_3, id_4) =
            (resource_1.id(), resource_2.id(), resource_3.id(), resource_4.id());

        let (delete_result_1, delete_result_2, delete_result_3, delete_result_4) = tokio::join!(
            time::timeout(Duration::from_secs(1), datalith.delete_resource_by_id(id_1)),
            time::timeout(Duration::from_secs(1), datalith.delete_resource_by_id(id_2)),
            time::timeout(Duration::from_secs(1), datalith.delete_resource_by_id(id_3)),
            time::timeout(Duration::from_secs(1), datalith.delete_resource_by_id(id_4)),
        );

        // timeout errors will be thrown
        assert!(delete_result_1.is_err());
        assert!(delete_result_2.is_err());
        // 3 or 4 will be deleted successfully because they are the same file and the **count** is 2. After deleting, the count will be updated to 1
        assert!(delete_result_3.is_err() ^ delete_result_4.is_err());

        drop(resource_1);
        drop(resource_2);
        drop(resource_3);
        drop(resource_4);

        let (delete_result_1, delete_result_2, delete_result_3, delete_result_4) = tokio::join!(
            datalith.delete_resource_by_id(id_1),
            datalith.delete_resource_by_id(id_2),
            datalith.delete_resource_by_id(id_3),
            datalith.delete_resource_by_id(id_4),
        );

        assert!(delete_result_1.unwrap());
        assert!(delete_result_2.unwrap());
        // 3 or 4 will be deleted successfully because one of them is already deleted
        assert!(delete_result_3.unwrap() ^ delete_result_4.unwrap());

        let (delete_result_1, delete_result_2, delete_result_3, delete_result_4) = tokio::join!(
            datalith.delete_resource_by_id(id_1),
            datalith.delete_resource_by_id(id_2),
            datalith.delete_resource_by_id(id_3),
            datalith.delete_resource_by_id(id_4),
        );

        assert!(!delete_result_1.unwrap());
        assert!(!delete_result_2.unwrap());
        assert!(!delete_result_3.unwrap());
        assert!(!delete_result_4.unwrap());
    }

    datalith_close(datalith).await;
}

#[cfg(feature = "image-convert")]
#[tokio::test]
async fn image_parallel() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let (image_1, image_2, image_3) = tokio::join!(
            datalith.put_image_by_buffer(
                image.to_vec(),
                Some("image.png"),
                Some(32),
                None,
                None,
                true
            ),
            datalith.put_image_by_buffer(
                image.to_vec(),
                Some("image.png"),
                Some(32),
                None,
                None,
                true
            ),
            datalith.put_image_by_buffer(
                image.to_vec(),
                Some("image.png"),
                Some(48),
                None,
                None,
                true
            ),
        );

        let image_1 = image_1.unwrap();
        let image_2 = image_2.unwrap();
        let image_3 = image_3.unwrap();

        assert_ne!(image_1, image_2);
        assert_ne!(image_1, image_3);
        assert_ne!(image_2, image_3);
        assert_eq!(image_1.original_file(), image_2.original_file());
        assert_eq!(image_1.original_file(), image_3.original_file());
        assert_eq!(image_1.thumbnails()[0], image_2.thumbnails()[0]);
        assert_ne!(image_1.thumbnails()[0], image_3.thumbnails()[0]);
        assert_eq!(image_1.thumbnails()[2], image_3.thumbnails()[1]);

        let (image_ids, _) = datalith.list_image_ids(PaginationOptions::default()).await.unwrap();
        assert_eq!(3, image_ids.len());

        let id_1 = image_1.id();
        let id_2 = image_2.id();
        let id_3 = image_3.id();

        let (delete_result_1, delete_result_2, delete_result_3) = tokio::join!(
            time::timeout(Duration::from_secs(5), datalith.delete_image_by_id(id_1)),
            time::timeout(Duration::from_secs(5), datalith.delete_image_by_id(id_2)),
            time::timeout(Duration::from_secs(1), datalith.delete_image_by_id(id_3)),
        );

        // timeout errors will be thrown
        // 1 or 2 will be deleted successfully because they are using the same files and the **count** is 2. After deleting, the count will be updated to 1
        assert!(delete_result_1.is_err() ^ delete_result_2.is_err());
        assert!(delete_result_3.is_err());

        drop(image_1);
        drop(image_2);
        drop(image_3);

        let (delete_result_1, delete_result_2, delete_result_3) = tokio::join!(
            datalith.delete_image_by_id(id_1),
            datalith.delete_image_by_id(id_2),
            datalith.delete_image_by_id(id_3),
        );

        // 1 or 2 will be deleted successfully because one of them is already deleted
        assert!(delete_result_1.unwrap() ^ delete_result_2.unwrap());
        assert!(delete_result_3.unwrap());

        let (delete_result_1, delete_result_2, delete_result_3) = tokio::join!(
            datalith.delete_image_by_id(id_1),
            datalith.delete_image_by_id(id_2),
            datalith.delete_image_by_id(id_3)
        );

        assert!(!delete_result_1.unwrap());
        assert!(!delete_result_2.unwrap());
        assert!(!delete_result_3.unwrap());
    }

    datalith_close(datalith).await;
}
