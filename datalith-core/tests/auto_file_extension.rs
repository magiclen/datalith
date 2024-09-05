mod global;

use global::*;
use tokio::fs::File;

#[tokio::test]
async fn auto_file_extension() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let id = {
            let file = datalith
                .put_file_by_buffer_temporarily(image, Some("MagicLen"), None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            #[cfg(feature = "magic")]
            assert_eq!("MagicLen.png", file.file_name());
            #[cfg(not(feature = "magic"))]
            assert_eq!("MagicLen.bin", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith.put_file_by_buffer(image, Some("MagicLen"), None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            #[cfg(feature = "magic")]
            assert_eq!("MagicLen.png", file.file_name());
            #[cfg(not(feature = "magic"))]
            assert_eq!("MagicLen.bin", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith
                .put_file_by_path_temporarily(IMAGE_PATH, Some("MagicLen"), None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!("MagicLen.png", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith.put_file_by_path(IMAGE_PATH, Some("MagicLen"), None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!("MagicLen.png", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let file = datalith
                .put_file_by_reader_temporarily(&mut file, Some("MagicLen"), None, Some(IMAGE_SIZE))
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            #[cfg(feature = "magic")]
            assert_eq!("MagicLen.png", file.file_name());
            #[cfg(not(feature = "magic"))]
            assert_eq!("MagicLen.bin", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let file = datalith
                .put_file_by_reader(&mut file, Some("MagicLen"), None, Some(IMAGE_SIZE))
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            #[cfg(feature = "magic")]
            assert_eq!("MagicLen.png", file.file_name());
            #[cfg(not(feature = "magic"))]
            assert_eq!("MagicLen.bin", file.file_name());

            file.id()
        };

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
    }

    datalith_close(datalith).await;
}

#[cfg(feature = "image-convert")]
#[tokio::test]
async fn image_auto_file_extension() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let id = {
            let image = datalith
                .put_image_by_buffer(image, Some("MagicLen"), Some(32), None, None, true)
                .await
                .unwrap();

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!("MagicLen.png", original_file.file_name());

            image.id()
        };

        // delete
        assert!(datalith.delete_image_by_id(id).await.unwrap());
    }

    {
        let id = {
            let image = datalith
                .put_image_by_path(IMAGE_PATH, Some("MagicLen"), Some(32), None, None, true)
                .await
                .unwrap();

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!("MagicLen.png", original_file.file_name());

            image.id()
        };

        // delete
        assert!(datalith.delete_image_by_id(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let image = datalith
                .put_image_by_reader(
                    &mut file,
                    Some("MagicLen"),
                    Some(32),
                    None,
                    None,
                    true,
                    Some(IMAGE_SIZE),
                )
                .await
                .unwrap();

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!("MagicLen.png", original_file.file_name());

            image.id()
        };

        // delete
        assert!(datalith.delete_image_by_id(id).await.unwrap());
    }

    datalith_close(datalith).await;
}
