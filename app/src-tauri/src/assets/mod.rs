//! `cover://` URI scheme — serves a downloaded album cover to the webview.
//!
//! Request shape: `cover://localhost/<album_id>` (or
//! `http://cover.localhost/<album_id>` on Windows/Android).
//!
//! Resolution: look up `album_art.local_cover_path` for the album; if
//! present and the file exists, serve it (200) with a guessed
//! `Content-Type`. Otherwise 404 so the `<img>` falls back to its
//! `onerror` placeholder.
//!
//! Why a custom protocol (like `media://`): the cover lives in app-private
//! storage and the webview can't read it directly. Routing through Rust
//! avoids shipping a file:// URL (which Tauri blocks by default) and keeps
//! the path out of the webview's URL bar.

use std::path::PathBuf;

use tauri::http::{header, HeaderValue, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::cache::repo;
use crate::AppStateHandle;

pub const SCHEME: &str = "cover";

/// Entry point invoked by the builder's protocol registration.
pub fn handle<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let response = dispatch(&app, &request).await;
        responder.respond(response);
    });
}

async fn dispatch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let album_id = match parse_album_id(request.uri().path()) {
        Some(id) => id,
        None => return text(StatusCode::BAD_REQUEST, "missing album id"),
    };

    let state = app.state::<AppStateHandle>();
    let row = match repo::get_album_art(&state.pool, &album_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return text(StatusCode::NOT_FOUND, "no local cover"),
        Err(e) => {
            tracing::warn!(err = %e, album_id = %album_id, "cover lookup failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "lookup failed");
        }
    };

    let path = PathBuf::from(&row.local_cover_path);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = guess_mime(&path);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_LENGTH, bytes.len().to_string())
                .header(header::CACHE_CONTROL, "max-age=3600")
                .body(bytes)
                .unwrap()
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), err = %e, "cover file missing");
            text(StatusCode::NOT_FOUND, "cover file not found")
        }
    }
}

fn parse_album_id(path: &str) -> Option<String> {
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

fn guess_mime(path: &std::path::Path) -> HeaderValue {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    };
    HeaderValue::from_str(mime).unwrap()
}

fn text<S: Into<String>>(status: StatusCode, body: S) -> Response<Vec<u8>> {
    let bytes = body.into().into_bytes();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(bytes)
        .unwrap()
}
