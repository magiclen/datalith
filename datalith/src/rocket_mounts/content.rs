use std::{
    io::{self, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use datalith_core::{
    Content, ContentRequest, DatalithService, HlsAsset, HlsAudioFilter, HlsPlaylist, ServiceError,
    Uuid, chrono::DateTime,
};
use rocket::{
    Request, Response, State,
    http::Status,
    request::{FromRequest, Outcome},
    response::{self, Responder},
};
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt, ReadBuf};
use url_escape::percent_encoding::AsciiSet;

use super::ApiError;

// RFC 8187 allows only these characters to stay unescaped in `filename*`.
const FILENAME_ATTR_CHARS: &AsciiSet = &url_escape::NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

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

// `None` means the header is ignored, which RFC 9110 allows for a range that is invalid or not supported.
fn parse_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ApiError> {
    let Some((unit, range)) = value.split_once('=') else {
        return Ok(None);
    };
    // Multiple ranges are not supported.
    if !unit.trim().eq_ignore_ascii_case("bytes") || range.contains(',') || size == 0 {
        return Ok(None);
    }
    let Some((start, end)) = range.trim().split_once('-') else {
        return Ok(None);
    };
    if start.is_empty() {
        let Some(suffix) = parse_position(end) else {
            return Ok(None);
        };
        if suffix == 0 {
            return Err(ApiError::range(size));
        }
        Ok(Some((size.saturating_sub(suffix), size - 1)))
    } else {
        let Some(first) = parse_position(start) else {
            return Ok(None);
        };
        let last = if end.is_empty() {
            size - 1
        } else {
            let Some(last) = parse_position(end) else {
                return Ok(None);
            };
            if last < first {
                return Ok(None);
            }
            last.min(size - 1)
        };
        if first >= size {
            return Err(ApiError::range(size));
        }
        Ok(Some((first, last)))
    }
}

fn parse_position(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // Only digits are left, so parsing fails only when the number is too large.
    Some(value.parse().unwrap_or(u64::MAX))
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
    url_escape::encode_to_string(
        &content.metadata.file_name,
        FILENAME_ATTR_CHARS,
        &mut disposition,
    );
    response.raw_header("Content-Disposition", disposition);
    response.raw_header(
        "Last-Modified",
        content.created_at.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
    );
    // Stop browsers from guessing the type or running scripts in uploaded files such as HTML or SVG.
    response.raw_header("X-Content-Type-Options", "nosniff");
    response.raw_header("Content-Security-Policy", "sandbox");
    let mut start = 0;
    let mut length = size;
    if content.repeatable {
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
            // RFC 9110 requires an exact match with `ETag` or `Last-Modified`.
            let apply_range = headers.if_range.as_deref().is_none_or(|value| {
                value == etag
                    || DateTime::parse_from_rfc2822(value)
                        .is_ok_and(|date| content.created_at.timestamp() == date.timestamp())
            });
            if apply_range
                && let Some(range) = headers.range
                && let Some((first, last)) = parse_range(&range, size)?
            {
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
    // Rocket reads 4 KiB at a time by default; this must come after `sized_body`, which replaces the body.
    response.max_chunk_size(64 * 1024);
    Ok(ContentResponse(response.finalize()))
}

#[utoipa::path(
    get,
    path = "/media/{id}/content",
    operation_id = "getContent",
    summary = "Read media content",
    tag = "Content",
    description = "Read a resource, image output, standalone audio output, or retained original. Video playback uses HLS routes. Resource and image single-use GET claims the full file and ignores Range and cache conditions; HEAD does not claim it. Single-use audio/video require an already claimed session token and support repeated reads and byte ranges during that session. HEAD ignores Range. Content is checked before cache conditions.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("variant" = Option<String>, Query, description = "Image recipe name or standalone audio identifier. Use original for the retained source; otherwise leave it out for the default available output."),
        ("multiplier" = Option<u64>, Query, description = "Image scale, default 1. Standalone audio accepts only 1.", minimum = 1, maximum = 255),
        ("format" = Option<super::openapi::ContentFormat>, Query, description = "Image format or standalone audio format. m4a and aac select AAC in M4A."),
        ("download" = Option<bool>, Query, example = false),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video and retained originals after claiming a playback session.")
    ),
    responses(
        (status = 200, description = "Full content", body = super::openapi::Binary, content_type = "*/*", headers(("ETag" = String, description = "Strong SHA-256 ETag. Resource and image single-use downloads omit it."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for ordinary or authorized session content. Resource and image single-use downloads omit it."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 206, description = "Requested byte range", body = super::openapi::Binary, content_type = "*/*", headers(("ETag" = String, description = "Strong SHA-256 ETag. Resource and image single-use downloads omit it."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for ordinary or authorized session content. Resource and image single-use downloads omit it."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag. Resource and image single-use downloads omit it."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for ordinary or authorized session content. Resource and image single-use downloads omit it."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 416, description = "The requested range is outside the file", body = super::openapi::Binary, content_type = "application/json", headers(("Content-Range" = String, description = "")))
    )
)]
#[get("/media/<id>/content?<variant>&<multiplier>&<format>&<download>&<session>")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn get_content(
    service: &State<DatalithService>,
    id: Uuid,
    variant: Option<String>,
    multiplier: Option<u8>,
    format: Option<String>,
    download: Option<bool>,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    let content = service
        .open_content_with_session(
            id,
            ContentRequest {
                variant,
                multiplier,
                format,
            },
            false,
            session,
        )
        .await?;
    response(content, headers, download.unwrap_or(false), false).await
}

#[utoipa::path(
    head,
    path = "/media/{id}/content",
    operation_id = "headContent",
    summary = "Read media content",
    tag = "Content",
    description = "Read a resource, image output, standalone audio output, or retained original. Video playback uses HLS routes. Resource and image single-use GET claims the full file and ignores Range and cache conditions; HEAD does not claim it. Single-use audio/video require an already claimed session token and support repeated reads and byte ranges during that session. HEAD ignores Range. Content is checked before cache conditions.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("variant" = Option<String>, Query, description = "Image recipe name or standalone audio identifier. Use original for the retained source; otherwise leave it out for the default available output."),
        ("multiplier" = Option<u64>, Query, description = "Image scale, default 1. Standalone audio accepts only 1.", minimum = 1, maximum = 255),
        ("format" = Option<super::openapi::ContentFormat>, Query, description = "Image format or standalone audio format. m4a and aac select AAC in M4A."),
        ("download" = Option<bool>, Query, example = false),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video and retained originals after claiming a playback session.")
    ),
    responses(
        (status = 200, description = "Full content", headers(("ETag" = String, description = "Strong SHA-256 ETag. Resource and image single-use downloads omit it."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for ordinary or authorized session content. Resource and image single-use downloads omit it."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag. Resource and image single-use downloads omit it."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for ordinary or authorized session content. Resource and image single-use downloads omit it."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = "")))
    )
)]
#[head("/media/<id>/content?<variant>&<multiplier>&<format>&<download>&<session>")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn head_content(
    service: &State<DatalithService>,
    id: Uuid,
    variant: Option<String>,
    multiplier: Option<u8>,
    format: Option<String>,
    download: Option<bool>,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    let content = service
        .open_content_with_session(
            id,
            ContentRequest {
                variant,
                multiplier,
                format,
            },
            true,
            session,
        )
        .await?;
    response(content, headers, download.unwrap_or(false), true).await
}

#[utoipa::path(
    get,
    path = "/tasks/{id}/artifact",
    operation_id = "getArtifact",
    summary = "Download a completed TAR or MP4 export",
    tag = "Content",
    description = "Read the artifact of a successful export task. MP4 exports expire separately from task-history retention, after 24 hours by default; the task record remains at least until artifact expiry. Single-use MP4 exports require the same still-valid playback session and media. Normal MP4 exports use an input snapshot and can complete after source deletion, while source expiry remains applicable. HEAD ignores Range. Completed artifact reads remain available without av-convert or FFmpeg processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("session" = Option<String>, Query, description = "Required for an MP4 artifact originating from single-use media.")
    ),
    responses(
        (status = 200, description = "Full content", body = super::openapi::Binary, content_type = "*/*", headers(("ETag" = String, description = "Strong SHA-256 ETag for the artifact."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for an authorized artifact."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 206, description = "Requested byte range", body = super::openapi::Binary, content_type = "*/*", headers(("ETag" = String, description = "Strong SHA-256 ETag for the artifact."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for an authorized artifact."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag for the artifact."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for an authorized artifact."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 416, description = "The requested range is outside the file", body = super::openapi::Binary, content_type = "application/json", headers(("Content-Range" = String, description = "")))
    )
)]
#[get("/tasks/<id>/artifact?<session>")]
pub(super) async fn get_artifact(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(service.open_artifact_with_session(id, session).await?, headers, true, false).await
}

#[utoipa::path(
    head,
    path = "/tasks/{id}/artifact",
    operation_id = "headArtifact",
    summary = "Read completed export headers",
    tag = "Content",
    description = "Read the artifact of a successful export task. MP4 exports expire separately from task-history retention, after 24 hours by default; the task record remains at least until artifact expiry. Single-use MP4 exports require the same still-valid playback session and media. Normal MP4 exports use an input snapshot and can complete after source deletion, while source expiry remains applicable. HEAD ignores Range. Completed artifact reads remain available without av-convert or FFmpeg processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("session" = Option<String>, Query, description = "Required for an MP4 artifact originating from single-use media.")
    ),
    responses(
        (status = 200, description = "Full content", headers(("ETag" = String, description = "Strong SHA-256 ETag for the artifact."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for an authorized artifact."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag for the artifact."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes for an authorized artifact."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = "")))
    )
)]
#[head("/tasks/<id>/artifact?<session>")]
pub(super) async fn head_artifact(
    service: &State<DatalithService>,
    id: Uuid,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(service.open_artifact_with_session(id, session).await?, headers, true, true).await
}

fn playlist_response(playlist: HlsPlaylist) -> ContentResponse {
    let bytes = playlist.body.into_bytes();
    let mut response = Response::build();
    response
        .raw_header("Content-Type", "application/vnd.apple.mpegurl")
        .raw_header("X-Content-Type-Options", "nosniff")
        .raw_header(
            "Cache-Control",
            if playlist.temporary { "no-store" } else { "public, max-age=0, must-revalidate" },
        );
    response.sized_body(bytes.len(), std::io::Cursor::new(bytes));
    ContentResponse(response.finalize())
}

fn audio_filter(audio: Option<&str>) -> Result<HlsAudioFilter, ApiError> {
    match audio.unwrap_or("aac") {
        "aac" => Ok(HlsAudioFilter::Aac),
        "all" => Ok(HlsAudioFilter::All),
        "flac" => Ok(HlsAudioFilter::Flac),
        _ => Err(ApiError::invalid("audio must be aac, all, or flac")),
    }
}

#[utoipa::path(
    get,
    path = "/media/{id}/hls/master.m3u8",
    operation_id = "getHlsMaster",
    summary = "Read the HLS master playlist",
    tag = "Content",
    description = "Return allowed video/audio combinations with measured bandwidth values. Default AAC combinations support broad playback; clients must check FLAC support before selecting it. Video and audio combinations reuse stored tracks. Playlist child requests retain the session credential when required. Child URLs are relative to the master, such as aac_low/index.m3u8. Only authorized single-use media append the session token; ordinary playlists do not echo supplied tokens.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("audio" = Option<super::openapi::HlsAudio>, Query, example = "aac"),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video. Ordinary media need no token.")
    ),
    responses(
        (status = 200, description = "HLS VOD playlist.", body = String, content_type = "application/vnd.apple.mpegurl", headers(("Cache-Control" = String, description = "Session-authorized or expiring media use no-store.")))
    )
)]
#[get("/media/<id>/hls/master.m3u8?<audio>&<session>")]
pub(super) async fn hls_master(
    service: &State<DatalithService>,
    id: Uuid,
    audio: Option<&str>,
    session: Option<&str>,
) -> Result<ContentResponse, ApiError> {
    Ok(playlist_response(service.hls_master(id, audio_filter(audio)?, session).await?))
}

#[utoipa::path(
    head,
    path = "/media/{id}/hls/master.m3u8",
    operation_id = "headHlsMaster",
    summary = "Read the HLS master playlist headers",
    tag = "Content",
    description = "Return allowed video/audio combinations with measured bandwidth values. Default AAC combinations support broad playback; clients must check FLAC support before selecting it. Video and audio combinations reuse stored tracks. Playlist child requests retain the session credential when required. Child URLs are relative to the master, such as aac_low/index.m3u8. Only authorized single-use media append the session token; ordinary playlists do not echo supplied tokens.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("audio" = Option<super::openapi::HlsAudio>, Query, example = "aac"),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video. Ordinary media need no token.")
    ),
    responses(
        (status = 200, description = "HLS VOD playlist.", headers(("Cache-Control" = String, description = "Session-authorized or expiring media use no-store.")))
    )
)]
#[head("/media/<id>/hls/master.m3u8?<audio>&<session>")]
pub(super) async fn head_hls_master(
    service: &State<DatalithService>,
    id: Uuid,
    audio: Option<&str>,
    session: Option<&str>,
) -> Result<ContentResponse, ApiError> {
    hls_master(service, id, audio, session).await
}

#[utoipa::path(
    get,
    path = "/media/{id}/hls/{track}/index.m3u8",
    operation_id = "getHlsTrack",
    summary = "Read one HLS track playlist",
    tag = "Content",
    description = "Read a video or shared audio track playlist. Child URLs are relative to this track, such as init.mp4 and segment-000000.m4s. Protected playlists append the same authorized session token to child URLs. Existing HLS remains readable without av-convert or processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("track" = String, Path, description = "An identifier from the video variants or shared audio summary."),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video. Ordinary media need no token.")
    ),
    responses(
        (status = 200, description = "HLS VOD playlist.", body = String, content_type = "application/vnd.apple.mpegurl", headers(("Cache-Control" = String, description = "Session-authorized or expiring media use no-store.")))
    )
)]
#[get("/media/<id>/hls/<track>/index.m3u8?<session>")]
pub(super) async fn hls_track(
    service: &State<DatalithService>,
    id: Uuid,
    track: &str,
    session: Option<&str>,
) -> Result<ContentResponse, ApiError> {
    Ok(playlist_response(service.hls_track(id, track, session).await?))
}

#[utoipa::path(
    head,
    path = "/media/{id}/hls/{track}/index.m3u8",
    operation_id = "headHlsTrack",
    summary = "Read one HLS track playlist headers",
    tag = "Content",
    description = "Read a video or shared audio track playlist. Child URLs are relative to this track, such as init.mp4 and segment-000000.m4s. Protected playlists append the same authorized session token to child URLs. Existing HLS remains readable without av-convert or processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("track" = String, Path, description = "An identifier from the video variants or shared audio summary."),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video. Ordinary media need no token.")
    ),
    responses(
        (status = 200, description = "HLS VOD playlist.", headers(("Cache-Control" = String, description = "Session-authorized or expiring media use no-store.")))
    )
)]
#[head("/media/<id>/hls/<track>/index.m3u8?<session>")]
pub(super) async fn head_hls_track(
    service: &State<DatalithService>,
    id: Uuid,
    track: &str,
    session: Option<&str>,
) -> Result<ContentResponse, ApiError> {
    hls_track(service, id, track, session).await
}

fn asset_kind(name: &str) -> Result<HlsAsset, ApiError> {
    if name == "init.mp4" {
        return Ok(HlsAsset::Initialization);
    }
    let value = name
        .strip_prefix("segment-")
        .and_then(|value| value.strip_suffix(".m4s"))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or(ServiceError::NotFound)?;
    Ok(HlsAsset::Segment(value))
}

#[utoipa::path(
    get,
    path = "/media/{id}/hls/{track}/{name}",
    operation_id = "getHlsAsset",
    summary = "Read an HLS initialization file",
    tag = "Content",
    description = "Open a published HLS track asset. GET supports one byte range and cache conditions after authorization; HEAD ignores Range. Single-use playback supports repeated reads with the same token and uses no-store. These reads remain available without audio/video processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("track" = String, Path, description = "An identifier from the video variants or shared audio summary."),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video, using its already claimed active playback token. Ordinary media need no token."),
        ("name" = String, Path, description = "init.mp4 or a segment-NNNNNN.m4s file from the track playlist.")
    ),
    responses(
        (status = 200, description = "Full content", body = super::openapi::Binary, content_type = "video/mp4", headers(("ETag" = String, description = "Strong SHA-256 ETag for the stored asset."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes, including session-authorized playback."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 206, description = "Requested byte range", body = super::openapi::Binary, content_type = "video/mp4", headers(("ETag" = String, description = "Strong SHA-256 ETag for the stored asset."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes, including session-authorized playback."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag for the stored asset."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes, including session-authorized playback."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 416, description = "The requested range is outside the file", body = super::openapi::Binary, content_type = "application/json", headers(("Content-Range" = String, description = "")))
    )
)]
#[get("/media/<id>/hls/<track>/<name>?<session>", rank = 2)]
pub(super) async fn hls_asset(
    service: &State<DatalithService>,
    id: Uuid,
    track: &str,
    name: &str,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(
        service.open_hls_content(id, track, asset_kind(name)?, session).await?,
        headers,
        false,
        false,
    )
    .await
}

#[utoipa::path(
    head,
    path = "/media/{id}/hls/{track}/{name}",
    operation_id = "headHlsAsset",
    summary = "Read headers for an HLS initialization file",
    tag = "Content",
    description = "Open a published HLS track asset. GET supports one byte range and cache conditions after authorization; HEAD ignores Range. Single-use playback supports repeated reads with the same token and uses no-store. These reads remain available without audio/video processing tools.",
    params(
        ("id" = datalith_core::Uuid, Path),
        ("track" = String, Path, description = "An identifier from the video variants or shared audio summary."),
        ("If-None-Match" = Option<String>, Header),
        ("If-Range" = Option<String>, Header),
        ("Range" = Option<String>, Header, description = "One byte range. It may have a start and end, only a start, or a suffix length to read from the end."),
        ("session" = Option<String>, Query, description = "Required for single-use audio/video, using its already claimed active playback token. Ordinary media need no token."),
        ("name" = String, Path, description = "init.mp4 or a segment-NNNNNN.m4s file from the track playlist.")
    ),
    responses(
        (status = 200, description = "Full content", headers(("ETag" = String, description = "Strong SHA-256 ETag for the stored asset."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes, including session-authorized playback."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = ""))),
        (status = 304, description = "The stored content matches the cache condition", headers(("ETag" = String, description = "Strong SHA-256 ETag for the stored asset."), ("Last-Modified" = String, description = "Content creation time in HTTP date format."), ("Cache-Control" = String, description = "Expiring and single-use content use no-store."), ("Accept-Ranges" = String, description = "bytes, including session-authorized playback."), ("Content-Disposition" = String, description = ""), ("Content-Range" = String, description = "")))
    )
)]
#[head("/media/<id>/hls/<track>/<name>?<session>", rank = 2)]
pub(super) async fn head_hls_asset(
    service: &State<DatalithService>,
    id: Uuid,
    track: &str,
    name: &str,
    session: Option<&str>,
    headers: DownloadHeaders,
) -> Result<ContentResponse, ApiError> {
    response(
        service.open_hls_content(id, track, asset_kind(name)?, session).await?,
        headers,
        false,
        true,
    )
    .await
}
