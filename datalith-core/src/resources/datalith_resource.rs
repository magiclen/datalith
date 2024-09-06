use chrono::{DateTime, Local, TimeZone};
use educe::Educe;
use mime::Mime;
use uuid::Uuid;

use crate::DatalithFile;

/// A struct that represents an image.
#[derive(Debug, Educe)]
#[educe(PartialEq, Eq, Hash)]
pub struct DatalithResource {
    id:           Uuid,
    #[educe(Eq(ignore), Hash(ignore))]
    created_at:   DateTime<Local>,
    #[educe(Eq(ignore), Hash(ignore))]
    file_type:    Mime,
    #[educe(Eq(ignore), Hash(ignore))]
    file_name:    String,
    #[educe(Eq(ignore), Hash(ignore))]
    file:         DatalithFile,
    #[educe(Eq(ignore), Hash(ignore))]
    is_temporary: bool,
}

impl DatalithResource {
    /// Create a resource instance.
    #[inline]
    pub(crate) fn new<Tz: TimeZone>(
        id: impl Into<Uuid>,
        created_at: DateTime<Tz>,
        file_type: Mime,
        file_name: impl Into<String>,
        file: DatalithFile,
        is_temporary: bool,
    ) -> Self
where {
        let id = id.into();
        let file_name = file_name.into();

        Self {
            id,
            created_at: created_at.with_timezone(&Local),
            file_type,
            file_name,
            file,
            is_temporary,
        }
    }
}

impl DatalithResource {
    /// Retrieve the file ID (UUID).
    #[inline]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Retrieve the creation time.
    #[inline]
    pub const fn created_at(&self) -> DateTime<Local> {
        self.created_at
    }

    /// Retrieve the file type (MIME).
    #[inline]
    pub const fn file_type(&self) -> &Mime {
        &self.file_type
    }

    /// Retrieve the file name.
    #[inline]
    pub const fn file_name(&self) -> &String {
        &self.file_name
    }

    /// Retrieve the file.
    #[inline]
    pub const fn file(&self) -> &DatalithFile {
        &self.file
    }

    /// Check if this resource is temporary.
    #[inline]
    pub const fn is_temporary(&self) -> bool {
        self.is_temporary
    }
}

impl From<DatalithResource> for DatalithFile {
    #[inline]
    fn from(value: DatalithResource) -> Self {
        value.file
    }
}
