mod global;

use std::time::Duration;

use datalith_core::PATH_FILE_DIRECTORY;
use global::*;
use tokio::{fs, time};

#[tokio::test]
async fn clear_expired_files() {
    let datalith = datalith_init().await;

    datalith.set_temporary_file_lifespan(Duration::from_secs(2));

    let image = IMAGE_DATA.as_ref();

    {
        let file =
            datalith.put_file_by_buffer_temporarily(image, Some("image.png"), None).await.unwrap();

        let file_id = file.id();

        drop(file);

        assert_eq!(0, datalith.clear_expired_files(Duration::from_millis(500)).await.unwrap());

        assert!(datalith.check_file_exist(file_id).await.unwrap());

        time::sleep(Duration::from_secs(2)).await;

        assert_eq!(1, datalith.clear_expired_files(Duration::from_millis(500)).await.unwrap());

        assert!(!datalith.check_file_exist(file_id).await.unwrap());
    }

    datalith_close(datalith).await;
}

#[tokio::test]
async fn clear_untracked_files() {
    let datalith = datalith_init().await;

    let environment = datalith.get_environment();

    {
        let image = IMAGE_DATA.as_ref();

        let id_1 = datalith
            .put_file_by_buffer_temporarily(image, Some("image.png"), None)
            .await
            .unwrap()
            .id();
        let id_2 = datalith.put_file_by_buffer(image, Some("image.png"), None).await.unwrap().id();

        assert_eq!(0, datalith.clear_untracked_files().await.unwrap());

        let file_path_hello = environment.join(PATH_FILE_DIRECTORY).join("hello.txt");
        let file_path_uuid =
            environment.join(PATH_FILE_DIRECTORY).join("70b7c850506e4fa98a4a713aca21f594");

        fs::write(file_path_hello.as_path(), b"Hello world!").await.unwrap();
        fs::write(file_path_uuid.as_path(), b"Hello world!").await.unwrap();

        assert!(fs::try_exists(file_path_hello.as_path()).await.unwrap());
        assert!(fs::try_exists(file_path_uuid.as_path()).await.unwrap());

        assert_eq!(2, datalith.clear_untracked_files().await.unwrap());

        assert!(!fs::try_exists(file_path_hello.as_path()).await.unwrap());
        assert!(!fs::try_exists(file_path_uuid.as_path()).await.unwrap());

        assert!(datalith.check_file_exist(id_1).await.unwrap());
        assert!(datalith.check_file_exist(id_2).await.unwrap());
    }

    datalith_close(datalith).await;
}
