pub(super) fn video_resolution() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Integer)
        .enum_values(Some([144, 240, 360, 432, 480, 540, 576, 720, 900, 1080, 1440, 2160]))
        .into()
}

pub(super) fn video_fps() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::Integer)
        .enum_values(Some([10, 12, 15, 20, 24, 25, 30, 48, 50, 60]))
        .into()
}

pub(super) fn variant_format() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some(["webp", "png", "jpeg", "gif"]))
        .into()
}

pub(super) fn audio_variant_codec() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some(["aac", "flac"]))
        .into()
}

pub(super) fn task_kind() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some([
            "upload",
            "resource",
            "image",
            "audio",
            "video",
            "import",
            "export",
            "mp4_export",
        ]))
        .into()
}

pub(super) fn process_options_kind() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .enum_values(Some(["image", "audio", "video"]))
        .into()
}

pub(super) fn mp4_export_result_audio() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::SchemaType::Array(vec![
            utoipa::openapi::schema::Type::String,
            utoipa::openapi::schema::Type::Null,
        ]))
        .enum_values(Some([
            serde_json::json!("aac_low"),
            serde_json::json!("aac_high"),
            serde_json::json!("flac"),
            serde_json::Value::Null,
        ]))
        .into()
}
