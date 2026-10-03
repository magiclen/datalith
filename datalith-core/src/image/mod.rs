mod datalith_image;
mod datalith_image_errors;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    str::FromStr,
    sync::{LazyLock, atomic::Ordering},
};

use chrono::{DateTime, Local};
pub use datalith_image::*;
pub use datalith_image_errors::*;
use educe::Educe;
use image_convert::{
    Crop, ImageResource, JPGConfig, MagickError, PNGConfig, WEBPConfig, compute_output_size,
    fetch_magic_wand, identify_ping, to_jpg, to_png, to_webp,
};
use mime::Mime;
use rdb_pagination::{Pagination, PaginationOptions, SqlJoin, SqlOrderByComponent, prelude::*};
use regex::Regex;
use tokio::{io::AsyncRead, task, task::JoinSet};
use uuid::Uuid;

use crate::{
    Datalith, DatalithFile, DatalithReadError, DatalithResource, FileTypeLevel,
    datalith::get_file_size_by_reader_and_copy_to_file,
    functions::get_file_name,
    guard::{DeleteGuard, TemporaryFileGuard},
};

pub static MIME_WEBP: LazyLock<Mime> = LazyLock::new(|| Mime::from_str("image/webp").unwrap());

/// Sort options for image queries.
#[derive(Debug, Clone, Educe, OrderByOptions)]
#[educe(Default)]
#[orderByOptions(name = images)]
pub struct DatalithImageOrderBy {
    #[educe(Default = 102)]
    #[orderByOptions((images, id), unique)]
    pub id:         OrderMethod,
    #[educe(Default = -101)]
    #[orderByOptions((images, created_at))]
    pub created_at: OrderMethod,
}

/// The width-to-height ratio for a center crop.
#[derive(Debug, Clone)]
pub struct CenterCrop(f64, f64);

impl CenterCrop {
    #[inline]
    pub fn new(w: f64, h: f64) -> Option<Self> {
        let r = w / h;

        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 || !r.is_finite() || r == 0.0 {
            None
        } else {
            Some(Self(w, h))
        }
    }
}

impl From<CenterCrop> for Crop {
    #[inline]
    fn from(value: CenterCrop) -> Self {
        Self::Center(value.0, value.1)
    }
}

impl Datalith {
    /// Get the maximum pixel count for an uploaded image.
    #[inline]
    pub fn get_max_image_resolution(&self) -> u32 {
        self.0._max_image_resolution.load(Ordering::Relaxed)
    }

    /// Set the maximum pixel count for an uploaded image.
    ///
    /// The minimum resolution is **1**.
    #[inline]
    pub fn set_max_image_resolution(&self, mut resolution: u32) {
        if resolution == 0 {
            resolution = 1;
        }

        self.0._max_image_resolution.swap(resolution, Ordering::Relaxed);
    }

    /// Get the maximum size multiplier for uploaded images.
    #[inline]
    pub fn get_max_image_resolution_multiplier(&self) -> u8 {
        self.0._max_image_resolution_multiplier.load(Ordering::Relaxed)
    }

    /// Set the maximum size multiplier for uploaded images.
    ///
    /// The minimum resolution multiplier is **1**.
    #[inline]
    pub fn set_max_image_resolution_multiplier(&self, mut resolution_multiplier: u8) {
        if resolution_multiplier == 0 {
            resolution_multiplier = 1;
        }

        self.0._max_image_resolution_multiplier.swap(resolution_multiplier, Ordering::Relaxed);
    }
}

// Upload
impl Datalith {
    /// Store an image from a buffer.
    pub async fn put_image_by_buffer(
        &self,
        buffer: impl Into<Vec<u8>>,
        file_name: Option<impl Into<String>>,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
        save_original_file: bool,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        // Create the input image.
        let input = ImageResource::Data(buffer.into());

        // Read the image metadata.
        let (input, input_width, input_height, file_type, has_alpha_channel) =
            self.read_image_metadata(input).await?;

        let file_name = file_name.map(|e| e.into());

        // Save the original file if requested.
        let (created_at, file_name, original_file) = if save_original_file {
            let original_file = self
                .put_file_by_buffer(
                    input.as_u8_slice().unwrap(),
                    file_name.clone(),
                    Some((file_type.clone(), FileTypeLevel::Manual)),
                )
                .await?;

            // An existing file keeps the name from its first upload, so use the new name here.
            let (created_at, file_name) = if original_file.is_new() {
                (original_file.created_at(), original_file.file_name().to_string())
            } else {
                let created_at = Local::now();

                (created_at, get_file_name(file_name, created_at, &file_type))
            };

            (created_at, file_name, Some(original_file))
        } else {
            let created_at = Local::now();

            (created_at, get_file_name(file_name, created_at, &file_type), None)
        };

        self.put_image(
            input,
            input_width,
            input_height,
            has_alpha_channel,
            created_at,
            file_name,
            original_file,
            max_width,
            max_height,
            center_crop,
        )
        .await
    }

    /// Store an image from a path.
    pub async fn put_image_by_path(
        &self,
        file_path: impl AsRef<Path>,
        file_name: Option<impl Into<String>>,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
        save_original_file: bool,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        let file_path = file_path.as_ref();
        let file_path_string = match file_path.to_str() {
            Some(file_path) => file_path.to_string(),
            None => {
                return Err(DatalithImageWriteError::MagickError(MagickError(String::from(
                    "unsupported path encoding",
                ))));
            },
        };

        // Create the input image.
        let input = ImageResource::Path(file_path_string);

        // Read the image metadata.
        let (input, input_width, input_height, file_type, has_alpha_channel) =
            self.read_image_metadata(input).await?;

        fn generate_file_name(
            file_name: Option<String>,
            file_path: &Path,
            created_at: DateTime<Local>,
            file_type: &Mime,
        ) -> String {
            if let Some(file_name) = file_name {
                get_file_name(Some(file_name), created_at, file_type)
            } else if let Some(file_name) = file_path.file_name() {
                file_name.to_string_lossy().into_owned()
            } else {
                unreachable!();
            }
        }

        let file_name = file_name.map(|e| e.into());

        let (created_at, file_name, original_file) = if save_original_file {
            let original_file = self
                .put_file_by_path(
                    file_path,
                    file_name.clone(),
                    Some((file_type.clone(), FileTypeLevel::Manual)),
                )
                .await?;

            let (created_at, file_name) = if original_file.is_new() {
                (original_file.created_at(), original_file.file_name().to_string())
            } else {
                let created_at = Local::now();
                let file_name = generate_file_name(file_name, file_path, created_at, &file_type);

                (created_at, file_name)
            };

            (created_at, file_name, Some(original_file))
        } else {
            let created_at = Local::now();
            let file_name = generate_file_name(file_name, file_path, created_at, &file_type);

            (created_at, file_name, None)
        };

        self.put_image(
            input,
            input_width,
            input_height,
            has_alpha_channel,
            created_at,
            file_name,
            original_file,
            max_width,
            max_height,
            center_crop,
        )
        .await
    }

    /// Store an image from a reader.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub async fn put_image_by_reader(
        &self,
        reader: impl AsyncRead + Unpin,
        file_name: Option<impl Into<String>>,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
        save_original_file: bool,
        expected_reader_length: Option<u64>,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        let temporary_file_path = self.get_temporary_file_path(Uuid::new_v4()).await?;

        get_file_size_by_reader_and_copy_to_file(
            reader,
            temporary_file_path.as_path(),
            expected_reader_length,
        )
        .await?;
        let _file_guard = TemporaryFileGuard::new(temporary_file_path.as_path());

        self.put_image_by_path(
            temporary_file_path,
            file_name,
            max_width,
            max_height,
            center_crop,
            save_original_file,
        )
        .await
    }

    /// Create an image using a resource.
    #[inline]
    pub async fn put_image_by_resource(
        &self,
        resource: &DatalithResource,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        let file = resource.file();
        let reader = file.create_reader().await?;

        self.put_image_by_reader(
            reader,
            Some(resource.file_name()),
            max_width,
            max_height,
            center_crop,
            true,
            Some(file.file_size()),
        )
        .await
    }

    /// Convert a resource into an image.
    #[inline]
    pub async fn convert_resource_to_image(
        &self,
        resource: DatalithResource,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        let image =
            self.put_image_by_resource(&resource, max_width, max_height, center_crop).await?;

        let resource_id = resource.id();

        drop(resource);

        match self.delete_resource_by_id(resource_id).await {
            Ok(_) => Ok(image),
            Err(error) => {
                // fallback

                let image_id = image.id();

                if let Err(error) = self.delete_image_by_id(image_id).await {
                    tracing::warn!(
                        "cannot fallback `convert_resource_to_image` (resource_id = \
                         {resource_id}, image_id = {image_id}): {error}"
                    );
                }

                Err(error.into())
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn put_image(
        &self,
        input: ImageResource,
        _input_width: u16,
        _input_height: u16,
        has_alpha_channel: bool,
        created_at: DateTime<Local>,
        file_name: String,
        original_file: Option<DatalithFile>,
        max_width: Option<u16>,
        max_height: Option<u16>,
        center_crop: Option<CenterCrop>,
    ) -> Result<DatalithImage, DatalithImageWriteError> {
        macro_rules! recover_original_file {
            () => {
                if let Some(original_file) = original_file {
                    let id = original_file.id();

                    drop(original_file);

                    self.delete_file_by_id(id).await?;
                }
            };
        }

        let center_crop = center_crop.map(|e| e.into());

        let prepared = task::spawn_blocking(move || {
            let config = WEBPConfig {
                crop: center_crop,
                respect_orientation: true,
                ..WEBPConfig::default()
            };
            let (wand, _) = fetch_magic_wand(&input, &config)?;
            let width = u16::try_from(wand.get_image_width())
                .map_err(|_| MagickError("The image width is too large.".into()))?;
            let height = u16::try_from(wand.get_image_height())
                .map_err(|_| MagickError("The image height is too large.".into()))?;
            Ok((ImageResource::MagickWand(wand), width, height)) as Result<_, MagickError>
        })
        .await
        .map_err(|error| MagickError(error.to_string()))?;
        let (mut input, input_width, input_height) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                recover_original_file!();
                return Err(error.into());
            },
        };

        let max_image_multiplier = self.get_max_image_resolution_multiplier() as usize;
        let mut thumbnails: Vec<DatalithFile> = Vec::with_capacity(max_image_multiplier); // webp files
        let mut fallback_thumbnails: Vec<DatalithFile> = Vec::with_capacity(max_image_multiplier); // fallback image files

        macro_rules! recover_thumbnails_and_original_files {
            () => {
                recover_original_file!();

                let mut tasks = JoinSet::new();

                for thumbnail in thumbnails.into_iter().chain(fallback_thumbnails) {
                    let id = thumbnail.id();

                    drop(thumbnail);

                    let datalith = self.clone();

                    tasks.spawn(async move { datalith.delete_file_by_id(id).await });
                }

                {
                    let mut final_error = None;

                    while let Some(result) = tasks.join_next().await {
                        if let Err(error) = result.unwrap() {
                            final_error = Some(error);
                        }
                    }

                    if let Some(error) = final_error {
                        return Err(error.into());
                    }
                }
            };
        }

        let (image_width, image_height) = match compute_output_size(
            true,
            u32::from(input_width),
            u32::from(input_height),
            u32::from(max_width.unwrap_or(0)),
            u32::from(max_height.unwrap_or(0)),
        ) {
            Some((width, height)) => (width as u16, height as u16),
            None => (input_width, input_height),
        };

        // A file name such as `.` or `..` has no stem.
        let fallback_stem = created_at.timestamp_millis().to_string();
        let file_stem = Path::new(file_name.as_str())
            .file_stem()
            .and_then(|file_stem| file_stem.to_str())
            .unwrap_or(fallback_stem.as_str());

        for image_multiplier in 1..=max_image_multiplier as u16 {
            let width = if let Some(width) = image_width.checked_mul(image_multiplier) {
                if width > input_width {
                    // the width is too large
                    break;
                }

                width
            } else {
                // the width is too large
                break;
            };
            let height = if let Some(height) = image_height.checked_mul(image_multiplier) {
                if height > input_height {
                    // the height is too large
                    break;
                }

                height
            } else {
                // the height is too large
                break;
            };

            let file = {
                let output = {
                    let (returned_input, result) = task::spawn_blocking(move || {
                        let result = (|| {
                            let mut output = ImageResource::Data(Vec::new());

                            let config = WEBPConfig {
                                width: u32::from(width),
                                height: u32::from(height),
                                respect_orientation: true,
                                quality: 80,
                                ..WEBPConfig::default()
                            };

                            to_webp(&mut output, &input, &config)?;

                            Ok(output.into_vec().unwrap()) as Result<Vec<u8>, MagickError>
                        })();
                        (input, result)
                    })
                    .await
                    .map_err(|error| MagickError(error.to_string()))?;
                    input = returned_input;

                    match result {
                        Ok(result) => result,
                        Err(error) => {
                            recover_thumbnails_and_original_files!();

                            return Err(error.into());
                        },
                    }
                };

                let file_name = format!("{file_stem}@{image_multiplier}x.webp");

                match self
                    .put_file_by_buffer(
                        output.as_slice(),
                        Some(file_name),
                        Some((MIME_WEBP.clone(), FileTypeLevel::Manual)),
                    )
                    .await
                {
                    Ok(file) => file,
                    Err(error) => {
                        recover_thumbnails_and_original_files!();

                        return Err(error.into());
                    },
                }
            };

            thumbnails.push(file);

            let fallback_file = {
                let (output, ext, file_type) = {
                    let (returned_input, result) = task::spawn_blocking(move || {
                        let result = (|| {
                            let mut output = ImageResource::Data(Vec::new());
                            let (ext, file_type) = if has_alpha_channel {
                                let config = PNGConfig {
                                    width: u32::from(width),
                                    height: u32::from(height),
                                    respect_orientation: true,
                                    ..PNGConfig::default()
                                };

                                to_png(&mut output, &input, &config)?;

                                ("png", mime::IMAGE_PNG)
                            } else {
                                let config = JPGConfig {
                                    width: u32::from(width),
                                    height: u32::from(height),
                                    respect_orientation: true,
                                    quality: Some(70),
                                    force_to_chroma_quartered: true,
                                    ..JPGConfig::default()
                                };

                                to_jpg(&mut output, &input, &config)?;

                                ("jpg", mime::IMAGE_JPEG)
                            };

                            Ok((output.into_vec().unwrap(), ext, file_type))
                                as Result<(Vec<u8>, &'static str, Mime), MagickError>
                        })();
                        (input, result)
                    })
                    .await
                    .map_err(|error| MagickError(error.to_string()))?;
                    input = returned_input;

                    match result {
                        Ok(result) => result,
                        Err(error) => {
                            recover_thumbnails_and_original_files!();

                            return Err(error.into());
                        },
                    }
                };

                let file_name = format!("{file_stem}_{image_multiplier}x.{ext}");

                match self
                    .put_file_by_buffer(
                        output.as_slice(),
                        Some(file_name),
                        Some((file_type, FileTypeLevel::Manual)),
                    )
                    .await
                {
                    Ok(file) => file,
                    Err(error) => {
                        recover_thumbnails_and_original_files!();

                        return Err(error.into());
                    },
                }
            };

            fallback_thumbnails.push(fallback_file);
        }

        let image_stem = {
            let file_stem = file_stem.trim();

            if file_stem.is_empty() {
                let image_name = thumbnails.first().unwrap().file_name();

                let image_stem = Path::new(image_name).file_stem().unwrap().to_str().unwrap();

                static RE_STEM: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"(.*?)(?:@\d+x)?$").unwrap());

                let captures = RE_STEM.captures(image_stem).unwrap();

                captures.get(1).unwrap().as_str()
            } else {
                file_stem
            }
        };

        let mut tx = match self.0.db.begin().await {
            Ok(tx) => tx,
            Err(error) => {
                recover_thumbnails_and_original_files!();

                return Err(error.into());
            },
        };

        let id = Uuid::new_v4();

        // Save the image metadata.
        {
            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    INSERT INTO `images` (`id`, `created_at`, `image_stem`, `image_width`, `image_height`, `original_file_id`, `has_alpha_channel`)
                        VALUES (?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(id)
            .bind(created_at.timestamp_millis())
            .bind(image_stem)
            .bind(image_width)
            .bind(image_height)
            .bind(original_file.as_ref().map(|e| e.id()))
            .bind(has_alpha_channel)
            .execute(&mut *tx)
            .await;

            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    drop(tx);

                    recover_thumbnails_and_original_files!();

                    return Err(error.into());
                },
            };

            debug_assert!(result.rows_affected() > 0);
        }

        // Save the thumbnail references.
        {
            const VALUES_PATTERN_CONCAT: &str = ", (?, ?, ?, ?), (?, ?, ?, ?)";

            let mut sql = String::from(
                "
                    INSERT INTO image_thumbnails (`image_id`, `multiplier`, `fallback`, `file_id`)
                            VALUES (?, ?, ?, ?), (?, ?, ?, ?)
                ",
            );

            let max_image_multiplier = thumbnails.len();

            for _ in 1..max_image_multiplier {
                sql.push_str(VALUES_PATTERN_CONCAT)
            }

            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));

            for (index, (thumbnail, fallback_thumbnail)) in
                thumbnails.iter().zip(fallback_thumbnails.iter()).enumerate()
            {
                let multiplier = index as u32 + 1;

                query = query
                    .bind(id)
                    .bind(multiplier)
                    .bind(false)
                    .bind(thumbnail.id())
                    .bind(id)
                    .bind(multiplier)
                    .bind(true)
                    .bind(fallback_thumbnail.id());
            }

            let result = query.execute(&mut *tx).await;

            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    drop(tx);

                    recover_thumbnails_and_original_files!();

                    return Err(error.into());
                },
            };

            debug_assert_eq!(max_image_multiplier as u64 * 2, result.rows_affected());
        }

        if let Err(error) = tx.commit().await {
            recover_thumbnails_and_original_files!();

            return Err(error.into());
        }

        let image = DatalithImage::new(
            id,
            created_at,
            image_stem.to_string(),
            image_width,
            image_height,
            original_file,
            thumbnails,
            fallback_thumbnails,
            has_alpha_channel,
        );

        Ok(image)
    }

    async fn read_image_metadata(
        &self,
        input: ImageResource,
    ) -> Result<(ImageResource, u16, u16, Mime, bool), DatalithImageWriteError> {
        let (input, result) = task::spawn_blocking(move || {
            let result = identify_ping(&input);
            (input, result)
        })
        .await
        .map_err(|error| MagickError(error.to_string()))?;
        let ident = result.map_err(|_| DatalithImageWriteError::UnsupportedImageType)?;

        // Check the image width and height.
        if ident.resolution.width > u16::MAX as u32 || ident.resolution.height > u16::MAX as u32 {
            return Err(DatalithImageWriteError::ResolutionTooBig);
        }

        let input_width = ident.resolution.width as u16;
        let input_height = ident.resolution.height as u16;
        let mime_type = format!("image/{}", ident.format.to_ascii_lowercase());
        let has_alpha_channel = ident.has_alpha_channel;

        // Check the image pixel count.
        if input_width as u32 * input_height as u32 > self.get_max_image_resolution() {
            return Err(DatalithImageWriteError::ResolutionTooBig);
        }

        Ok((
            input,
            input_width,
            input_height,
            Mime::from_str(&mime_type).unwrap(),
            has_alpha_channel,
        ))
    }
}

// Download
impl Datalith {
    /// Check whether the image exists.
    pub async fn check_image_exist(&self, id: impl Into<Uuid>) -> Result<bool, DatalithReadError> {
        #[rustfmt::skip]
        let row = sqlx::query(
            "
                SELECT
                    1
                FROM
                    `images`
                WHERE
                    `id` = ?
            ",
        )
        .bind(id.into())
        .fetch_optional(&self.0.db)
        .await?;

        Ok(row.is_some())
    }

    /// Get the image metadata using an ID.
    pub async fn get_image_by_id(
        &self,
        image_id: impl Into<Uuid>,
    ) -> Result<Option<DatalithImage>, DatalithReadError> {
        let image_id = image_id.into();

        #[rustfmt::skip]
        let image_thumbnails_rows: Vec<(Uuid,)> = sqlx::query_as(
            "
                SELECT
                    `file_id`
                FROM
                    `image_thumbnails`
                WHERE
                    `image_id` = ?
                ORDER BY
                    `multiplier` ASC,
                    `fallback` ASC
            ",
        )
        .bind(image_id)
        .fetch_all(&self.0.db)
        .await?;

        if image_thumbnails_rows.is_empty() {
            return Ok(None);
        }

        #[allow(clippy::type_complexity)]
        #[rustfmt::skip]
        let row: Option<(i64, String, u16, u16, Option<Uuid>, bool)> = sqlx::query_as(
            "
                SELECT
                    `created_at`,
                    `image_stem`,
                    `image_width`,
                    `image_height`,
                    `original_file_id`,
                    `has_alpha_channel`
                FROM
                    `images`
                WHERE
                    `id` = ?
            ",
        )
        .bind(image_id)
        .fetch_optional(&self.0.db)
        .await?;

        if let Some((
            created_at,
            image_stem,
            image_width,
            image_height,
            original_file_id,
            has_alpha_channel,
        )) = row
        {
            let original_file = if let Some(original_file_id) = original_file_id {
                self.get_file_by_id(original_file_id).await?
            } else {
                None
            };

            let (thumbnails, fallback_thumbnails) = {
                let max_image_resolution_multiplier = image_thumbnails_rows.len().div_ceil(2);
                let mut thumbnails = Vec::with_capacity(max_image_resolution_multiplier);
                let mut fallback_thumbnails = Vec::with_capacity(max_image_resolution_multiplier);

                for (thumbnail_id, fallback_thumbnail_id) in
                    image_thumbnails_rows.as_chunks::<2>().0.iter().map(|e| (e[0].0, e[1].0))
                {
                    let (thumbnail_result, fallback_thumbnail_result) = tokio::join!(
                        self.get_file_by_id(thumbnail_id),
                        self.get_file_by_id(fallback_thumbnail_id)
                    );

                    match thumbnail_result? {
                        Some(thumbnail) => thumbnails.push(thumbnail),
                        None => return Ok(None),
                    };

                    match fallback_thumbnail_result? {
                        Some(fallback_thumbnail) => fallback_thumbnails.push(fallback_thumbnail),
                        None => return Ok(None),
                    };
                }

                (thumbnails, fallback_thumbnails)
            };

            let created_at = DateTime::from_timestamp_millis(created_at).unwrap();

            let image = DatalithImage::new(
                image_id,
                created_at,
                image_stem,
                image_width,
                image_height,
                original_file,
                thumbnails,
                fallback_thumbnails,
                has_alpha_channel,
            );

            Ok(Some(image))
        } else {
            Ok(None)
        }
    }

    /// List image IDs.
    pub async fn list_image_ids(
        &self,
        mut pagination_options: PaginationOptions<DatalithImageOrderBy>,
    ) -> Result<(Vec<Uuid>, Pagination), DatalithReadError> {
        loop {
            let (joins, order_by_components) = pagination_options.order_by.to_sql();

            let mut sql_join = String::new();
            let mut sql_order_by = String::new();
            let mut sql_limit_offset = String::new();

            SqlJoin::format_sqlite_join_clauses(&joins, &mut sql_join);
            SqlOrderByComponent::format_sqlite_order_by_components(
                &order_by_components,
                &mut sql_order_by,
            );
            pagination_options.to_sqlite_limit_offset(&mut sql_limit_offset);

            let mut tx = self.0.db.begin().await?;

            let total_items = {
                let row: (u32,) = {
                    #[rustfmt::skip]
                    let query = sqlx::query_as(
                        "
                            SELECT
                                COUNT(*)
                            FROM
                                `images`
                        "
                    );

                    query.fetch_one(&mut *tx).await?
                };

                row.0
            };

            let rows: Vec<(Uuid,)> = {
                #[rustfmt::skip]
                let sql = format!(
                    "
                        SELECT
                            `id`
                        FROM
                            `images`
                        {sql_join}
                        {sql_order_by}
                        {sql_limit_offset}
                    "
                );

                let query = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()));

                query.fetch_all(&mut *tx).await?
            };

            let total_items = total_items as usize;

            drop(tx);

            let pagination = Pagination::new()
                .items_per_page(pagination_options.items_per_page)
                .total_items(total_items)
                .page(pagination_options.page);

            if rows.is_empty() {
                if total_items > 0 && pagination_options.page > 1 {
                    pagination_options.page = pagination.get_total_pages();

                    continue;
                } else {
                    return Ok((Vec::new(), pagination));
                }
            }

            let ids = rows.into_iter().map(|(id,)| id).collect::<Vec<Uuid>>();

            return Ok((ids, pagination));
        }
    }
}

// Delete
impl Datalith {
    /// Remove an image by ID.
    /// Drop all related `DatalithImage` values before calling this function.
    pub async fn delete_image_by_id(&self, id: impl Into<Uuid>) -> Result<bool, DatalithReadError> {
        let id = id.into();
        let image = self.get_image_by_id(id).await?;

        if let Some(image) = image {
            let mut references: HashMap<Uuid, u64> =
                HashMap::with_capacity(image.thumbnails().len() * 2 + 1);

            for file in image.thumbnails().iter().chain(image.fallback_thumbnails()) {
                *references.entry(file.id()).or_default() += 1;
            }

            if let Some(original_file) = image.original_file() {
                *references.entry(original_file.id()).or_default() += 1;
            }

            drop(image);

            let file_ids: HashSet<Uuid> = references.keys().copied().collect();
            let mut guards: Vec<DeleteGuard> = Vec::with_capacity(file_ids.len());
            DeleteGuard::acquire_multiple(&mut guards, self.clone(), &file_ids).await;

            if !self.check_image_exist(id).await? {
                return Ok(false);
            }

            for guard in guards.iter() {
                self.wait_for_opening_file_references(guard, references[&guard.id]).await?;
            }

            let mut tx = self.0.db.begin().await?;

            #[rustfmt::skip]
            sqlx::query(
                "
                    DELETE FROM
                        `image_thumbnails`
                    WHERE
                        `image_id` = ?
                ",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;

            #[rustfmt::skip]
            let result = sqlx::query(
                "
                    DELETE FROM
                        `images`
                    WHERE
                        `id` = ?
                ",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;

            if result.rows_affected() == 0 {
                return Ok(false);
            }

            let mut removed = Vec::new();
            for (file_id, count) in references {
                if Self::release_file_references_in_transaction(&mut tx, file_id, count).await? {
                    removed.push(file_id);
                }
            }

            tx.commit().await?;

            for file_id in removed {
                self.remove_untracked_file(file_id).await?;
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }
}
