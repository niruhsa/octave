//! REST streaming endpoint: `GET /tracks/{id}/stream`.
//!
//! - `200 OK` for a full-file request, `206 Partial Content` for any
//!   `Range:` request that parses + satisfies.
//! - `416 Range Not Satisfiable` (with `Content-Range: bytes */size`)
//!   when the range parses but falls outside the file. Per RFC 7233
//!   that's the signal clients use to recover.
//! - A malformed `Range` header is ignored and we serve the full body
//!   (RFC 7233 §3.1).
//! - `HEAD` is supported by axum's router contract automatically (we
//!   return the same headers; body construction stays the same shape).
//!
//! The body uses `tokio_util::io::ReaderStream` so we don't load the
//! file into memory; the file handle is seeked to the range start and
//! then `take(len)` bounds how many bytes go on the wire.

use std::time::SystemTime;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LAST_MODIFIED, RANGE,
        },
    },
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;
use tracing::warn;
use uuid::Uuid;

use crate::auth::Identity;
use crate::error::AppError;
use crate::rest::range::{RangeParseError, parse_range};
use crate::rest::{ApiError, RestState};
use crate::services::streaming::ResolvedStream;

pub fn router() -> Router<RestState> {
    Router::new().route("/tracks/:id/stream", get(stream_track).head(stream_track))
}

async fn stream_track(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    req: axum::extract::Request,
) -> Result<Response, ApiError> {
    let identity = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?
        .clone();
    let method = req.method().clone();

    let resolved = state.streaming.resolve(&identity, id).await?;

    // Pull the Range header (if any) and decide the response shape.
    let range_header = headers.get(RANGE).and_then(|h| h.to_str().ok());
    let parsed = range_header.map(|h| parse_range(h, resolved.size));

    let mut response_headers = base_headers(&resolved);

    let (status, start, end) = match parsed {
        Some(Ok(r)) => (StatusCode::PARTIAL_CONTENT, *r.start(), *r.end()),
        Some(Err(RangeParseError::Unsatisfiable)) => {
            // 416 must carry `Content-Range: bytes */<size>`.
            let mut h = HeaderMap::new();
            h.insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", resolved.size))
                    .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
            );
            h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, h).into_response());
        }
        // Malformed `Range` → ignore and serve full body.
        Some(Err(RangeParseError::Malformed)) | None => {
            (StatusCode::OK, 0u64, resolved.size.saturating_sub(1))
        }
    };

    let len = end - start + 1;
    response_headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).expect("ascii"),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response_headers.insert(
            CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", resolved.size))
                .expect("ascii"),
        );
    }

    // HEAD: return headers only.
    if method == axum::http::Method::HEAD {
        return Ok((status, response_headers).into_response());
    }

    // Empty file edge case — Content-Length: 0 and no body work for 200.
    if resolved.size == 0 {
        return Ok((status, response_headers).into_response());
    }

    let body = build_body(&resolved, start, len).await.map_err(|e| {
        warn!(track = %id, error = %e, "open file for streaming failed");
        ApiError(AppError::Internal("stream open failed".into()))
    })?;

    Ok((status, response_headers, body).into_response())
}

fn base_headers(resolved: &ResolvedStream) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(resolved.content_type));
    h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(modified) = resolved.modified {
        if let Some(val) = http_date(modified) {
            h.insert(LAST_MODIFIED, val);
        }
    }
    h
}

fn http_date(t: SystemTime) -> Option<HeaderValue> {
    let formatted = httpdate::fmt_http_date(t);
    HeaderValue::from_str(&formatted).ok()
}

async fn build_body(resolved: &ResolvedStream, start: u64, len: u64) -> std::io::Result<Body> {
    let mut file = tokio::fs::File::open(&resolved.path).await?;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    // `take` enforces the upper bound at the reader layer so we never
    // overshoot the requested range, even if the file grows mid-stream.
    let limited = file.take(len);
    Ok(Body::from_stream(ReaderStream::new(limited)))
}
