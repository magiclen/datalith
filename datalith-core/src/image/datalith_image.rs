use chrono::{DateTime, Local, TimeZone};
use educe::Educe;
use uuid::Uuid;

use crate::DatalithFile;

/// An image with its original file and thumbnails.
#[derive(Debug, Educe)]
#[educe(PartialEq, Eq, Hash)]
pub struct DatalithImage {
    id:                  Uuid,
    #[educe(PartialEq(ignore), Hash(ignore))]
    created_at:          DateTime<Local>,
    #[educe(PartialEq(ignore), Hash(ignore))]
    image_stem:          String,
    #[educe(PartialEq(ignore), Hash(ignore))]
    image_width:         u16,
    #[educe(PartialEq(ignore), Hash(ignore))]
    image_height:        u16,
    #[educe(PartialEq(ignore), Hash(ignore))]
    original_file:       Option<DatalithFile>,
    #[educe(PartialEq(ignore), Hash(ignore))]
    thumbnails:          Vec<DatalithFile>,
    #[educe(PartialEq(ignore), Hash(ignore))]
    fallback_thumbnails: Vec<DatalithFile>,
    #[educe(PartialEq(ignore), Hash(ignore))]
    has_alpha_channel:   bool,
}

impl DatalithImage {
    #[allow(clippy::too_many_arguments)]
    /// Create an image value.
    #[inline]
    pub(crate) fn new<Tz: TimeZone>(
        id: impl Into<Uuid>,
        created_at: DateTime<Tz>,
        image_stem: impl Into<String>,
        image_width: u16,
        image_height: u16,
        original_file: Option<DatalithFile>,
        thumbnails: Vec<DatalithFile>,
        fallback_thumbnails: Vec<DatalithFile>,
        has_alpha_channel: bool,
    ) -> Self
where {
        let id = id.into();
        let image_stem = image_stem.into();

        Self {
            id,
            created_at: created_at.with_timezone(&Local),
            image_stem,
            image_width,
            image_height,
            original_file,
            thumbnails,
            fallback_thumbnails,
            has_alpha_channel,
        }
    }
}

impl DatalithImage {
    /// Get the image ID (UUID).
    #[inline]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Get the creation time.
    #[inline]
    pub const fn created_at(&self) -> DateTime<Local> {
        self.created_at
    }

    /// Get the image file name without its extension.
    #[inline]
    pub const fn image_stem(&self) -> &String {
        &self.image_stem
    }

    /// Get the width of the 1x image.
    #[inline]
    pub const fn image_width(&self) -> u16 {
        self.image_width
    }

    /// Get the height of the 1x image.
    #[inline]
    pub const fn image_height(&self) -> u16 {
        self.image_height
    }

    /// Get the original file.
    #[inline]
    pub const fn original_file(&self) -> Option<&DatalithFile> {
        self.original_file.as_ref()
    }

    /// Get the WebP thumbnails.
    #[inline]
    pub const fn thumbnails(&self) -> &Vec<DatalithFile> {
        &self.thumbnails
    }

    /// Get the PNG or JPEG fallback thumbnails.
    #[inline]
    pub const fn fallback_thumbnails(&self) -> &Vec<DatalithFile> {
        &self.fallback_thumbnails
    }

    /// Check whether the image has an alpha channel.
    /// An alpha channel uses PNG fallbacks; otherwise, the fallbacks use JPEG.
    #[inline]
    pub const fn has_alpha_channel(&self) -> bool {
        self.has_alpha_channel
    }
}

impl DatalithImage {
    /// Take ownership of the original file.
    #[inline]
    pub fn into_original_file(self) -> Option<DatalithFile> {
        self.original_file
    }

    /// Take ownership of the WebP thumbnails.
    #[inline]
    pub fn into_thumbnails(self) -> Vec<DatalithFile> {
        self.thumbnails
    }

    /// Take ownership of the PNG or JPEG fallback thumbnails.
    #[inline]
    pub fn into_fallback_thumbnails(self) -> Vec<DatalithFile> {
        self.fallback_thumbnails
    }
}
