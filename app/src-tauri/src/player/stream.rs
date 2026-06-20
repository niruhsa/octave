//! `media://` protocol handler — the heart of Phase 4 playback.
//!
//! Request shape: `media://localhost/<track_id>` (or
//! `http://media.localhost/<track_id>` on Windows/Android).
//!
//! Resolution per request:
//! 1. **Local file** — if the track id is in the SQLite cache (i.e. the
//!    user downloaded it), serve `local_file_path` directly with full
//!    RFC 7233 byte-range semantics (200 / 206 / 416). This is the
//!    offline path and the "prefer local" online path.
//! 2. **Server stream** — otherwise proxy `GET /tracks/{id}/stream` from
//!    the server, injecting the active `Authorization` header and
//!    forwarding the incoming `Range` header. The server's 206 / 200 /
//!    416 response (headers + body) is relayed verbatim.
//! 3. **Offline + not cached** — if the server is unreachable and the
//!    track isn't local, return 502 so the `<audio>` element fires
//!    `error` and the UI can surface "not available offline".
//!
//! Why a custom protocol instead of pointing `<audio>` at the server URL
//! directly: the webview can't attach an `Authorization` header to a
//! media-element request, and we don't want to ship the session token in
//! a query param. Routing through Rust lets us attach credentials server-
//! side and keep the token out of the webview's URL bar / history.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::auth::AuthManager;
use crate::cache::repo;
use crate::error::AppResult;
use crate::AppStateHandle;

pub const SCHEME: &str = "media";

/// Entry point invoked by the builder's protocol registration.
///
/// Generic over the runtime so it works with the default `Wry` runtime
/// and any custom one. We clone the `AppHandle` out of the context and
/// do the real work on the async runtime.
pub fn handle<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let response = dispatch(&app, request).await;
        responder.respond(response);
    });
}

async fn dispatch<R: tauri::Runtime>(app: &tauri::AppHandle<R>, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let track_id = match parse_track_id(request.uri().path()) {
        Some(id) => id,
        None => return text(StatusCode::BAD_REQUEST, "missing track id"),
    };

    let state = app.state::<AppStateHandle>();

    // 1. Local cache hit → serve the file with range support.
    match repo::get_track(&state.pool, &track_id).await {
        Ok(Some(row)) => {
            return serve_local_file(&row.local_file_path, request.headers()).await;
        }
        Ok(None) => { /* fall through to server stream */ }
        Err(e) => {
            tracing::warn!(err = %e, track_id = %track_id, "cache lookup failed; trying server");
        }
    }

    // 2. Server stream — need an auth manager + credential.
    let auth = state.auth.read().await.clone();
    let Some(auth): Option<Arc<AuthManager>> = auth else {
        return text(StatusCode::UNAUTHORIZED, "not configured — log in first");
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return text(StatusCode::UNAUTHORIZED, "no active session"),
    };
    match proxy_server_stream(&auth.server_config(), &cred, &track_id, request.headers()).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(err = %e, track_id = %track_id, "server stream failed");
            // 502 = bad gateway: we couldn't reach the authority and the
            // track isn't cached locally. Distinct from 404 so the UI can
            // tell "offline, not downloaded" apart from "track missing".
            text(StatusCode::BAD_GATEWAY, "stream unavailable (offline and not downloaded)")
        }
    }
}

/// Strip the leading `/` and percent-decode the track id from the URI path.
fn parse_track_id(path: &str) -> Option<String> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(percent_decode(trimmed))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Local file serving
// ---------------------------------------------------------------------------

async fn serve_local_file(path: &str, headers: &HeaderMap) -> Response<Vec<u8>> {
    let path = PathBuf::from(path);
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), err = %e, "local media file missing");
            return text(StatusCode::NOT_FOUND, "local file not found");
        }
    };
    let total = metadata.len();
    let content_type = guess_mime(&path);

    // Range header (optional). Format: `bytes=START-END` (END optional).
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);

    let mut builder = Response::builder().header(header::ACCEPT_RANGES, "bytes");

    let (status, body, content_range): (StatusCode, Vec<u8>, Option<String>) = match range {
        Some((start, end)) if start < total => {
            let end = end.min(total - 1);
            let len = (end - start + 1) as usize;
            let mut buf = vec![0u8; len];
            let mut f = match tokio::fs::File::open(&path).await {
                Ok(f) => f,
                Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, format!("open: {e}")),
            };
            use std::io::SeekFrom;
            if let Err(e) = f.seek(SeekFrom::Start(start)).await {
                return text(StatusCode::INTERNAL_SERVER_ERROR, format!("seek: {e}"));
            }
            if let Err(e) = f.read_exact(&mut buf).await {
                return text(StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}"));
            }
            (
                StatusCode::PARTIAL_CONTENT,
                buf,
                Some(format!("bytes {start}-{end}/{total}")),
            )
        }
        Some(_) => {
            // Range unsatisfiable (start >= total).
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .header(header::CONTENT_TYPE, content_type)
                .body(Vec::new())
                .unwrap();
        }
        None => {
            // No range → full file.
            let buf = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}")),
            };
            (StatusCode::OK, buf, None)
        }
    };

    builder = builder
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len().to_string());
    if let Some(cr) = content_range {
        builder = builder.header(header::CONTENT_RANGE, cr);
    }
    builder.body(body).unwrap()
}

/// Parse `bytes=START-END` → `(start, end)`. `end` defaults to "EOF".
fn parse_range(raw: &str) -> Option<(u64, u64)> {
    let s = raw.strip_prefix("bytes=")?;
    let (start_s, end_s) = s.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    // `end_s` empty → suffix-from-start is invalid per RFC for this form;
    // treat as "to EOF" via u64::MAX, clamped by the caller.
    let end: u64 = if end_s.is_empty() {
        u64::MAX
    } else {
        end_s.parse().ok()?
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

fn guess_mime(path: &PathBuf) -> HeaderValue {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext.to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" | "mp4" | "aac" => "audio/mp4",
        "webm" => "audio/webm",
        _ => "application/octet-stream",
    };
    HeaderValue::from_str(mime).unwrap()
}

// ---------------------------------------------------------------------------
// Server stream proxy
// ---------------------------------------------------------------------------

/// One shared HTTP client for all stream proxying. Rebuilding a client per
/// range request (the `<audio>` element issues many) would thrash the TLS
/// session cache and add latency — audible as stutter on seek.
fn proxy_client() -> AppResult<&'static reqwest::Client> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| crate::error::AppError::Transport(format!("proxy client: {e}")))?;
    Ok(CLIENT.get_or_init(|| c))
}

async fn proxy_server_stream(
    config: &crate::transport::ServerConfig,
    cred: &crate::transport::Credential,
    track_id: &str,
    headers: &HeaderMap,
) -> AppResult<Response<Vec<u8>>> {
    let client = proxy_client()?;

    let url = format!("{}/tracks/{}/stream", config.rest_root(), track_id);
    let mut req = client
        .get(&url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?);

    // Forward the Range header so the server can answer 206.
    if let Some(range) = headers.get(header::RANGE) {
        req = req.header(header::RANGE, range.clone());
    }

    let resp = req
        .send()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("proxy send: {e}")))?;

    let status = resp.status();
    let mut builder = Response::builder().status(status);

    // Relay the headers the `<audio>` element cares about.
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::LAST_MODIFIED,
    ] {
        if let Some(v) = resp.headers().get(&name) {
            builder = builder.header(name, v.clone());
        }
    }

    let body = resp
        .bytes()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("proxy body: {e}")))?;
    Ok(builder.body(body.to_vec()).unwrap())
}

fn auth_header_value(cred: &crate::transport::Credential) -> AppResult<HeaderValue> {
    let s = match cred {
        crate::transport::Credential::SecretKey(k) => format!("SecretKey {k}"),
        crate::transport::Credential::Bearer(t) => format!("Bearer {t}"),
    };
    HeaderValue::from_str(&s).map_err(|_| crate::error::AppError::Internal("bad auth chars".into()))
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn text<S: Into<String>>(status: StatusCode, body: S) -> Response<Vec<u8>> {
    let bytes = body.into().into_bytes();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(bytes)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_full() {
        assert_eq!(parse_range("bytes=0-1023"), Some((0, 1023)));
    }

    #[test]
    fn parse_range_open_end() {
        assert_eq!(parse_range("bytes=500-"), Some((500, u64::MAX)));
    }

    #[test]
    fn parse_range_rejects_garbage() {
        assert_eq!(parse_range("seconds=0-10"), None);
        assert_eq!(parse_range("bytes=abc"), None);
        assert_eq!(parse_range("bytes=10-5"), None);
    }

    #[test]
    fn decode_track_id() {
        assert_eq!(percent_decode("abc"), "abc");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn parse_track_id_strips_slash() {
        assert_eq!(parse_track_id("/deadbeef"), Some("deadbeef".into()));
        assert_eq!(parse_track_id("/"), None);
        assert_eq!(parse_track_id(""), None);
    }
}
