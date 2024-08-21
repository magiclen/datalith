mod global;

use global::*;
use tokio::{fs, fs::File, io::AsyncReadExt};

#[tokio::test]
async fn upload_download_data() {
    let datalith = datalith_init().await;

    let image = fs::read(IMAGE_PATH).await.unwrap();

    {
        let id = {
            let file =
                datalith.put_file_by_buffer_temporarily(&image, "image.png", None).await.unwrap();

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
        assert!(!datalith.check_file_item_exist(id).await.unwrap());
    }

    {
        let id = {
            let file = datalith.put_file_by_buffer(&image, "image.png", None).await.unwrap();

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
        assert!(datalith.check_file_item_exist(id).await.unwrap());

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
        assert!(!datalith.check_file_item_exist(id).await.unwrap());
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
        assert!(datalith.check_file_item_exist(id).await.unwrap());

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
        assert!(!datalith.check_file_item_exist(id).await.unwrap());
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
        assert!(datalith.check_file_item_exist(id).await.unwrap());

        // delete
        assert!(datalith.delete_file_by_id(id).await.unwrap());
        assert!(!datalith.delete_file_by_id(id).await.unwrap());
    }

    datalith_close(datalith).await;
}
