mod global;

use std::time::Duration;

use global::*;
use tokio::{fs, time};

#[tokio::test]
async fn parallel_and_temporary() {
    let datalith = datalith_init().await;

    let image = fs::read(IMAGE_PATH).await.unwrap();

    {
        let (file_1, file_2, file_3, file_4) = tokio::join!(
            datalith.put_file_by_buffer_temporarily(&image, "image.png", None),
            datalith.put_file_by_buffer_temporarily(&image, "image.png", None),
            datalith.put_file_by_buffer(&image, "image.png", None),
            datalith.put_file_by_buffer(&image, "image.png", None),
        );

        let file_1 = file_1.unwrap();
        let file_2 = file_2.unwrap();
        let file_3 = file_3.unwrap();
        let file_4 = file_4.unwrap();

        assert_ne!(file_1, file_2);
        assert_ne!(file_1, file_3);
        assert_ne!(file_2, file_3);
        assert_eq!(file_3, file_4);

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
        // 3 or 4 will delete successfully because they are the same file and the **count** is 2. After deleting, the count will be updated to 1
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
            datalith.delete_file_by_id(id_3_4)
        );

        assert!(!delete_result_1.unwrap());
        assert!(!delete_result_2.unwrap());
        assert!(!delete_result_3_4.unwrap());
    }

    datalith_close(datalith).await;
}
