use std::{
    io::{self, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use datalith_core::{
    Content, ContentRequest, DatalithService, ServiceError, Uuid, chrono::DateTime,
};
use rocket::{
    Request, Response, State,
    http::Status,
    request::{FromRequest, Outcome},
    response::{self, Responder},
};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt, ReadBuf};

use super::ApiError;

pub(super) struct DownloadHeaders {
    range:         Option<String>,
    if_range:      Option<String>,
    if_none_match: Option<String>,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for DownloadHeaders {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self {
            range:         request.headers().get_one("Range").map(str::to_owned),
            if_range:      request.headers().get_one("If-Range").map(str::to_owned),
            if_none_match: request.headers().get_one("If-None-Match").map(str::to_owned),
        })
    }
}

pub(super) struct ContentResponse(Response<'static>);

impl<'r> Responder<'r, 'static> for ContentResponse {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        Ok(self.0)
    }
}

// The response keeps the content guard alive while the file is being read.
struct ContentReader {
    content:  Content,
    start:    u64,
    length:   u64,
    position: u64,
}

impl AsyncRead for ContentReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let remaining = this.length.saturating_sub(this.position);
        let amount = usize::try_from(remaining).unwrap_or(usize::MAX).min(buf.remaining());
        if amount == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut limited = ReadBuf::new(buf.initialize_unfilled_to(amount));
        match Pin::new(&mut this.content.file).poll_read(cx, &mut limited) {
            Poll::Ready(Ok(())) => {
                let count = limited.filled().len();
                buf.advance(count);
                this.position += count as u64;
                Poll::Ready(Ok(()))
            },
            other => other,
        }
    }
}

impl AsyncSeek for ContentReader {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        let this = self.get_mut();
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(this.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(this.length) + i128::from(offset),
        };
        let absolute = u64::try_from(position)
            .ok()
            .and_then(|position| this.start.checked_add(position))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Invalid content position")
            })?;
        Pin::new(&mut this.content.file).start_seek(SeekFrom::Start(absolute))
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        let this = self.get_mut();
        match Pin::new(&mut this.content.file).poll_complete(cx) {
            Poll::Ready(Ok(position)) => {
                this.position = position.saturating_sub(this.start);
                Poll::Ready(Ok(this.position))
            },
            other => other,
        }
    }
}

fn parse_range(value: &str, size: u64) -> Result<(u64, u64), ApiError> {
    let range = value.strip_prefix("bytes=").ok_or_else(|| ApiError::range(size))?;
    let (start, end) = range.split_once('-').ok_or_else(|| ApiError::range(size))?;
    if size == 0 || range.contains(',') {
        return Err(ApiError::range(size));
    }
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ApiError::range(size))?;
        if suffix == 0 {
            return Err(ApiError::range(size));
        }
        Ok((size.saturating_sub(suffix), size - 1))
    } else {
        let start = start.parse::<u64>().map_err(|_| ApiError::range(size))?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().map_err(|_| ApiError::range(size))?.min(size - 1)
        };
        if start >= size || end < start {
            return Err(ApiError::range(size));
        }
        Ok((start, end))
    }
}

async fn response(
    mut content: Content,
    headers: DownloadHeaders,
    download: bool,
    head: bool,
) -> Result<ContentResponse, ApiError> {
    let size = content
        .metadata
        .file_size
        .parse::<u64>()
        .map_err(|_| ServiceError::Internal("Invalid stored file size".into()))?;
    let etag = format!("\"{}\"", content.metadata.sha256);
    let mut response = Response::build();
    response.raw_header("Content-Type", content.metadata.file_type.clone());
    response.raw_header(
        "Cache-Control",
        if content.temporary || content.single_use {
            "no-store"
        } else {
            "public, max-age=0, must-revalidate"
        },
    );
    let mut disposition =
        format!("{}; filename*=UTF-8''", if download { "attachment" } else { "inline" });
    url_escape::encode_component_to_string(&content.metadata.file_name, &mut disposition);
    response.raw_header("Content-Disposition", disposition);
    response.raw_header(
        "Last-Modified",
        content.created_at.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
    );
    let mut start = 0;
    let mut length = size;
    if !content.single_use {
        response.raw_header("ETag", etag.clone()).raw_header("Accept-Ranges", "bytes");
        if headers.if_none_match.as_deref().is_some_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
            })
        }) {
            response.status(Status::NotModified);
            return Ok(ContentResponse(response.finalize()));
        }
        if !head {
            let apply_range = headers.if_range.as_deref().is_none_or(|value| {
                value == etag
                    || DateTime::parse_from_rfc2822(value)
                        .is_ok_and(|date| content.created_at.timestamp() <= date.timestamp())
            });
            if apply_range && let Some(range) = headers.range {
                let (first, last) = parse_range(&range, size)?;
                start = first;
                length = last - first + 1;
                response
                    .status(Status::PartialContent)
                    .raw_header("Content-Range", format!("bytes {first}-{last}/{size}"));
            }
        }
    }
    if start > 0 {
        content.file.seek(SeekFrom::Start(start)).await.map_err(ServiceError::from)?;
    }
    response.sized_body(usize::try_from(length).ok(), ContentReader {
        content,
        start,
        length,
        position: 0,
    });
    Ok(ContentResponse(response.finalize()))
}

#[get("/media/<id>/content?<variant>&<multiplier>&<format>&<download>")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn get_content(
    service: &State<DatalithService>,
    id: Uuid,
    variant: Option<String>,
    multiplier: Option<u8>,
    format: Option<String>,
    download: Option<bool>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    let content = service
        .open_content(
            id,
            ContentRequest {
                variant,
                multiplier,
                format,
            },
            false,
        )
        .await?;
    response(content, headers, download.unwrap_or(false), false).await
}

#[head("/media/<id>/content?<variant>&<multiplier>&<format>&<download>")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn head_content(
    service: &State<DatalithService>,
    id: Uuid,
    variant: Option<String>,
    multiplier: Option<u8>,
    format: Option<String>,
    download: Option<bool>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    let content = service
        .open_content(
            id,
            ContentRequest {
                variant,
                multiplier,
                format,
            },
            true,
        )
        .await?;
    response(content, headers, download.unwrap_or(false), true).await
}

#[get("/tasks/<id>/artifact")]
pub(super) async fn get_artifact(
    service: &State<DatalithService>,
    id: Uuid,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(service.open_artifact(id).await?, headers, true, false).await
}

#[head("/tasks/<id>/artifact")]
pub(super) async fn head_artifact(
    service: &State<DatalithService>,
    id: Uuid,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(service.open_artifact(id).await?, headers, true, true).await
}
