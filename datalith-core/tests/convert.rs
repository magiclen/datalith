#![cfg(feature = "image-convert")]

mod global;

use global::*;

#[tokio::test]
pub async fn put_image_by_resource() {
    let datalith = datalith_init().await;

    let image_data = IMAGE_DATA.as_ref();

    let resource =
        datalith.put_resource_by_buffer(image_data, Some("image.png"), None).await.unwrap();

    let image = datalith.put_image_by_resource(&resource, Some(32), None, None).await.unwrap();
    assert_eq!("image", image.image_stem());
    assert_eq!(32, image.image_width());
    assert_eq!(32, image.image_height());
    assert!(image.has_alpha_channel());
    assert_eq!(resource.file(), image.original_file().unwrap());

    let resource_id = resource.id();
    let image_id = image.id();

    assert!(datalith.check_resource_exist(resource_id).await.unwrap());
    assert!(datalith.check_image_exist(image_id).await.unwrap());

    datalith_close(datalith).await;
}

#[tokio::test]
pub async fn convert_resource_to_image() {
    let datalith = datalith_init().await;

    let image_data = IMAGE_DATA.as_ref();

    let resource =
        datalith.put_resource_by_buffer(image_data, Some("image.png"), None).await.unwrap();
    let resource_id = resource.id();

    let image = datalith.convert_resource_to_image(resource, Some(32), None, None).await.unwrap();
    assert_eq!("image", image.image_stem());
    assert_eq!(32, image.image_width());
    assert_eq!(32, image.image_height());
    assert!(image.has_alpha_channel());

    let image_id = image.id();

    assert!(!datalith.check_resource_exist(resource_id).await.unwrap());
    assert!(datalith.check_image_exist(image_id).await.unwrap());

    datalith_close(datalith).await;
}
