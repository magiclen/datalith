use chrono::{DateTime, Local, TimeZone};
use educe::Educe;
use uuid::Uuid;

use crate::DatalithFile;

/// A struct that represents an image.
#[derive(Debug, Educe)]
#[educe(PartialEq, Eq, Hash)]
pub struct DatalithImage {
    id:                  Uuid,
    #[educe(Eq(ignore), Hash(ignore))]
    created_at:          DateTime<Local>,
    #[educe(Eq(ignore), Hash(ignore))]
    image_stem:          String,
    #[educe(Eq(ignore), Hash(ignore))]
    image_width:         u16,
    #[educe(Eq(ignore), Hash(ignore))]
    image_height:        u16,
    #[educe(Eq(ignore), Hash(ignore))]
    original_file:       Option<DatalithFile>,
    #[educe(Eq(ignore), Hash(ignore))]
    thumbnails:          Vec<DatalithFile>,
    #[educe(Eq(ignore), Hash(ignore))]
    fallback_thumbnails: Vec<DatalithFile>,
    #[educe(Eq(ignore), Hash(ignore))]
    has_alpha_channel:   bool,
}

impl DatalithImage {
    #[allow(clippy::too_many_arguments)]
    /// Create an image instance.
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
    /// Retrieve the image ID (UUID).
    #[inline]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Retrieve the creation time.
    #[inline]
    pub const fn created_at(&self) -> DateTime<Local> {
        self.created_at
    }

    /// Retrieve the file stem of this image.
    #[inline]
    pub const fn image_stem(&self) -> &String {
        &self.image_stem
    }

    /// Retrieve the width of the 1x image.
    #[inline]
    pub const fn image_width(&self) -> u16 {
        self.image_width
    }

    /// Retrieve the height of the 1x image.
    #[inline]
    pub const fn image_height(&self) -> u16 {
        self.image_height
    }

    /// Retrieve the original file.
    #[inline]
    pub const fn original_file(&self) -> Option<&DatalithFile> {
        self.original_file.as_ref()
    }

    /// Retrieve the thumbnails. (WebP)
    #[inline]
    pub const fn thumbnails(&self) -> &Vec<DatalithFile> {
        &self.thumbnails
    }

    /// Retrieve the fallback thumbnails. (PNG or JPEG)
    #[inline]
    pub const fn fallback_thumbnails(&self) -> &Vec<DatalithFile> {
        &self.fallback_thumbnails
    }

    /// Check if the image has transparency. If it does, the fallback thumbnails are in PNG format; otherwise, they are in JPEG format.
    #[inline]
    pub const fn has_alpha_channel(&self) -> bool {
        self.has_alpha_channel
    }
}

impl DatalithImage {
    /// Convert to the original file.
    #[inline]
    pub fn into_original_file(self) -> Option<DatalithFile> {
        self.original_file
    }

    /// Convert to the thumbnails. (WebP)
    #[inline]
    pub fn into_thumbnails(self) -> Vec<DatalithFile> {
        self.thumbnails
    }

    /// Convert to the fallback thumbnails. (PNG or JPEG)
    #[inline]
    pub fn into_fallback_thumbnails(self) -> Vec<DatalithFile> {
        self.fallback_thumbnails
    }
}
