use chrono::{DateTime, Local, TimeZone};
use educe::Educe;
use mime::Mime;
use uuid::Uuid;

use crate::DatalithFile;

/// A resource that refers to a stored file.
#[derive(Debug, Educe)]
#[educe(PartialEq, Eq, Hash)]
pub struct DatalithResource {
    id:           Uuid,
    #[educe(PartialEq(ignore), Hash(ignore))]
    created_at:   DateTime<Local>,
    #[educe(PartialEq(ignore), Hash(ignore))]
    file_type:    Mime,
    #[educe(PartialEq(ignore), Hash(ignore))]
    file_name:    String,
    #[educe(PartialEq(ignore), Hash(ignore))]
    file:         DatalithFile,
    #[educe(PartialEq(ignore), Hash(ignore))]
    is_temporary: bool,
}

impl DatalithResource {
    /// Create a resource value.
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
    /// Get the resource ID (UUID).
    #[inline]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Get the creation time.
    #[inline]
    pub const fn created_at(&self) -> DateTime<Local> {
        self.created_at
    }

    /// Get the file type (MIME).
    #[inline]
    pub const fn file_type(&self) -> &Mime {
        &self.file_type
    }

    /// Get the file name.
    #[inline]
    pub const fn file_name(&self) -> &String {
        &self.file_name
    }

    /// Get the file.
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
