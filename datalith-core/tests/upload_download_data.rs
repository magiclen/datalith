mod global;

use global::*;
#[cfg(feature = "image-convert")]
use rdb_pagination::PaginationOptions;
use tokio::{fs::File, io::AsyncReadExt};

#[tokio::test]
async fn upload_download_data() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let id = {
            let file =
                datalith.put_file_by_buffer_temporarily(image, "image.png", None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily files can only get once
        assert!(datalith.get_file_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_file_exist(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith.put_file_by_buffer(image, "image.png", None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_file_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_file_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
        assert!(!datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith
                .put_file_by_path_temporarily(IMAGE_PATH, None::<&str>, None)
                .await
                .unwrap();

            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily files can only get once
        assert!(datalith.get_file_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_file_exist(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith.put_file_by_path(IMAGE_PATH, None::<&str>, None).await.unwrap();

            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_file_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_file_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
        assert!(!datalith.delete_file_by_id(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let file = datalith
                .put_file_by_reader_temporarily(&mut file, "image.png", None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily files can only get once
        assert!(datalith.get_file_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_file_exist(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let file = datalith.put_file_by_reader(&mut file, "image.png", None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            file.id()
        };

        // get
        {
            let file = datalith.get_file_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_file_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_file_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
        assert!(!datalith.delete_file_by_id(id).await.unwrap());
    }

    datalith_close(datalith).await;
}

#[cfg(feature = "image-convert")]
#[tokio::test]
async fn image_upload_download_data() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let id = {
            let image = datalith
                .put_image_by_buffer(image.to_vec(), "image.png", Some(32), None, None, true)
                .await
                .unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            image.id()
        };

        // get
        let original_file_id = {
            let image = datalith.get_image_by_id(id).await.unwrap().unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            original_file.id()
        };

        assert!(datalith.get_image_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_image_exist(id).await.unwrap());

        // delete
        assert!(!datalith.delete_file_by_id(original_file_id).await.unwrap());
        assert!(datalith.delete_image_by_id(id).await.unwrap());
        assert!(!datalith.delete_image_by_id(id).await.unwrap());
        assert!(datalith.list_file_ids(PaginationOptions::default()).await.unwrap().0.is_empty());
    }

    {
        let id = {
            let image = datalith
                .put_image_by_path(IMAGE_PATH, None::<&str>, Some(32), None, None, true)
                .await
                .unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            image.id()
        };

        // get
        let original_file_id = {
            let image = datalith.get_image_by_id(id).await.unwrap().unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            original_file.id()
        };

        assert!(datalith.get_image_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_image_exist(id).await.unwrap());

        // delete
        assert!(!datalith.delete_file_by_id(original_file_id).await.unwrap());
        assert!(datalith.delete_image_by_id(id).await.unwrap());
        assert!(!datalith.delete_image_by_id(id).await.unwrap());
        assert!(datalith.list_file_ids(PaginationOptions::default()).await.unwrap().0.is_empty());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let image = datalith
                .put_image_by_reader(&mut file, "image.png", Some(32), None, None, true)
                .await
                .unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            image.id()
        };

        // get
        let original_file_id = {
            let image = datalith.get_image_by_id(id).await.unwrap().unwrap();

            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());

            let original_file = image.original_file().unwrap();
            assert_eq!(&mime::IMAGE_PNG, original_file.file_type());
            assert_eq!(IMAGE_SIZE, original_file.file_size());
            assert_eq!("image.png", original_file.file_name());

            let thumbnails = image.thumbnails();
            let fallback_thumbnails = image.fallback_thumbnails();
            assert_eq!(3, thumbnails.len());
            assert_eq!(3, fallback_thumbnails.len());

            original_file.id()
        };

        assert!(datalith.get_image_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_image_exist(id).await.unwrap());

        // delete
        assert!(!datalith.delete_file_by_id(original_file_id).await.unwrap());
        assert!(datalith.delete_image_by_id(id).await.unwrap());
        assert!(!datalith.delete_image_by_id(id).await.unwrap());
        assert!(datalith.list_file_ids(PaginationOptions::default()).await.unwrap().0.is_empty());
    }

    datalith_close(datalith).await;
}
