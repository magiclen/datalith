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
            let file = datalith
                .put_file_by_buffer_temporarily(image, Some("image.png"), None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(file.is_new());

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
            assert!(file.is_temporary());
            assert!(!file.is_new());

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
            let file = datalith.put_file_by_buffer(image, Some("image.png"), None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(file.is_new());

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
            assert!(!file.is_temporary());
            assert!(!file.is_new());

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
            assert!(file.is_temporary());
            assert!(file.is_new());

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
            assert!(file.is_temporary());
            assert!(!file.is_new());

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
            assert!(!file.is_temporary());
            assert!(file.is_new());

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
            assert!(!file.is_temporary());
            assert!(!file.is_new());

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
                .put_file_by_reader_temporarily(
                    &mut file,
                    Some("image.png"),
                    None,
                    Some(IMAGE_SIZE),
                )
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(file.is_new());

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
            assert!(file.is_temporary());
            assert!(!file.is_new());

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

            let file = datalith
                .put_file_by_reader(&mut file, Some("image.png"), None, Some(IMAGE_SIZE))
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(file.is_new());

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
            assert!(!file.is_temporary());
            assert!(!file.is_new());

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

#[tokio::test]
async fn resource_upload_download_data() {
    let datalith = datalith_init().await;

    let image = IMAGE_DATA.as_ref();

    {
        let id = {
            let resource = datalith
                .put_resource_by_buffer_temporarily(image, Some("image.png"), None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily resources can only get once
        assert!(datalith.get_resource_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_resource_exist(id).await.unwrap());
    }

    {
        let id = {
            let resource =
                datalith.put_resource_by_buffer(image, Some("image.png"), None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_resource_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_resource_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_resource_by_id(id).await.unwrap());
        assert!(!datalith.delete_resource_by_id(id).await.unwrap());
    }

    {
        let id = {
            let resource = datalith
                .put_resource_by_path_temporarily(IMAGE_PATH, None::<&str>, None)
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily resources can only get once
        assert!(datalith.get_resource_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_resource_exist(id).await.unwrap());
    }

    {
        let id = {
            let resource =
                datalith.put_resource_by_path(IMAGE_PATH, None::<&str>, None).await.unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_resource_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_resource_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_resource_by_id(id).await.unwrap());
        assert!(!datalith.delete_resource_by_id(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let resource = datalith
                .put_resource_by_reader_temporarily(
                    &mut file,
                    Some("image.png"),
                    None,
                    Some(IMAGE_SIZE),
                )
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        // temporarily resources can only get once
        assert!(datalith.get_resource_by_id(id).await.unwrap().is_none());
        assert!(!datalith.check_resource_exist(id).await.unwrap());
    }

    {
        let id = {
            let mut file = File::open(IMAGE_PATH).await.unwrap();

            let resource = datalith
                .put_resource_by_reader(&mut file, Some("image.png"), None, Some(IMAGE_SIZE))
                .await
                .unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);

            resource.id()
        };

        // get
        {
            let resource = datalith.get_resource_by_id(id).await.unwrap().unwrap();

            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, resource.file_type());
            assert_eq!("image.png", resource.file_name());

            let file = resource.file();
            #[cfg(feature = "magic")]
            assert_eq!(&mime::IMAGE_PNG, file.file_type());
            assert_eq!(IMAGE_SIZE, file.file_size());
            assert_eq!("image.png", file.file_name());
            assert!(!file.is_temporary());
            assert!(!file.is_new());

            let mut reader = file.create_reader().await.unwrap();
            let mut buffer = Vec::with_capacity(file.file_size() as usize);
            reader.read_to_end(&mut buffer).await.unwrap();
            assert_eq!(image, buffer);
        }

        assert!(datalith.get_resource_by_id(id).await.unwrap().is_some());
        assert!(datalith.check_resource_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_resource_by_id(id).await.unwrap());
        assert!(!datalith.delete_resource_by_id(id).await.unwrap());
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
                .put_image_by_buffer(image.to_vec(), Some("image.png"), Some(32), None, None, true)
                .await
                .unwrap();

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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
                .put_image_by_reader(
                    &mut file,
                    Some("image.png"),
                    Some(32),
                    None,
                    None,
                    true,
                    Some(IMAGE_SIZE),
                )
                .await
                .unwrap();

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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

            assert_eq!("image", image.image_stem());
            assert_eq!(32, image.image_width());
            assert_eq!(32, image.image_height());
            assert!(image.has_alpha_channel());

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
