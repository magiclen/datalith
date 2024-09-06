use std::{
    io,
    io::SeekFrom,
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};

use chrono::{DateTime, Local, TimeZone};
use educe::Educe;
use mime::Mime;
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncSeek, ReadBuf},
};
use uuid::Uuid;

use crate::{guard::OpenGuard, Datalith};

/// A struct that represents a file.
#[derive(Debug, Educe)]
#[educe(PartialEq, Eq, Hash)]
pub struct DatalithFile {
    #[educe(Eq(ignore), Hash(ignore))]
    _datalith:    Datalith,
    #[educe(Eq(ignore), Hash(ignore))]
    _guard:       OpenGuard,
    id:           Uuid,
    #[educe(Eq(ignore), Hash(ignore))]
    created_at:   DateTime<Local>,
    #[educe(Eq(ignore), Hash(ignore))]
    file_size:    u64,
    #[educe(Eq(ignore), Hash(ignore))]
    file_type:    Mime,
    #[educe(Eq(ignore), Hash(ignore))]
    file_name:    String,
    #[educe(Eq(ignore), Hash(ignore))]
    is_temporary: bool,
    #[educe(Eq(ignore), Hash(ignore))]
    is_new:       bool,
}

impl DatalithFile {
    /// Create a file instance.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub(crate) fn new<Tz: TimeZone>(
        datalith: Datalith,
        guard: OpenGuard,
        id: impl Into<Uuid>,
        created_at: DateTime<Tz>,
        file_size: impl Into<u64>,
        file_type: Mime,
        file_name: impl Into<String>,
        is_temporary: bool,
        is_new: bool,
    ) -> Self
where {
        let id = id.into();

        Self {
            _datalith: datalith,
            _guard: guard,
            id,
            created_at: created_at.with_timezone(&Local),
            file_size: file_size.into(),
            file_type,
            file_name: file_name.into(),
            is_temporary,
            is_new,
        }
    }
}

impl DatalithFile {
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

    /// Retrieve the file size (in bytes).
    #[inline]
    pub const fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Retrieve the file name.
    #[inline]
    pub const fn file_name(&self) -> &String {
        &self.file_name
    }

    /// Check if this file is temporary.
    #[inline]
    pub const fn is_temporary(&self) -> bool {
        self.is_temporary
    }

    /// Check if this file is a new file.
    #[inline]
    pub const fn is_new(&self) -> bool {
        self.is_new
    }
}

impl DatalithFile {
    /// Create an reader.
    #[inline]
    pub async fn create_reader(&self) -> io::Result<DatalithFileReader> {
        let file_path = self._datalith.get_file_path(self.id).await?;

        let file = File::open(file_path).await?;

        Ok(DatalithFileReader {
            _file: self,
            file,
        })
    }

    /// Create a readable .
    #[inline]
    pub async fn into_readable(self) -> io::Result<ReadableDatalithFile> {
        let file_path = self._datalith.get_file_path(self.id).await?;

        let file = File::open(file_path).await?;

        Ok(ReadableDatalithFile {
            _file: self,
            file,
        })
    }
}

/// A struct that provides an asynchronous read interface for files.
#[derive(Debug)]
pub struct DatalithFileReader<'a> {
    _file: &'a DatalithFile,
    file:  File,
}

impl<'a> AsyncRead for DatalithFileReader<'a> {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

/// A struct that represents a file and provides an asynchronous read interface for files.
#[derive(Debug)]
pub struct ReadableDatalithFile {
    _file: DatalithFile,
    file:  File,
}

impl Deref for ReadableDatalithFile {
    type Target = DatalithFile;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self._file
    }
}

impl AsyncRead for ReadableDatalithFile {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

impl AsyncSeek for ReadableDatalithFile {
    #[inline]
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        Pin::new(&mut self.file).start_seek(position)
    }

    #[inline]
    fn poll_complete(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Pin::new(&mut self.file).poll_complete(cx)
    }
}
