//! `cover://` URI scheme — serves a downloaded album cover to the webview.
//!
//! Request shape: `cover://localhost/<album_id>` (or
//! `http://cover.localhost/<album_id>` on Windows/Android).
//!
//! Resolution (prefer-local-else-proxy, mirroring `media://`):
//!   1. **Local cover** — look up `album_art.local_cover_path`; if present
//!      and the file exists, serve it (200) with a guessed `Content-Type`.
//!   2. **Server proxy** — otherwise proxy the server's auth-gated
//!      `GET /albums/{id}/cover` with the session credential injected, so
//!      online (non-downloaded) albums still render their artwork. The
//!      webview can't call that endpoint itself (it needs the auth header,
//!      which we keep out of the webview), so we relay it here.
//!   3. Otherwise 404 so the `<img>` falls back to its `onerror`
//!      placeholder.
//!
//! Why a custom protocol (like `media://`): the cover lives in app-private
//! storage and the webview can't read it directly. Routing through Rust
//! avoids shipping a file:// URL (which Tauri blocks by default) and keeps
//! the path out of the webview's URL bar.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::http::{header, HeaderValue, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::auth::AuthManager;
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

    // 1. Local downloaded cover — serve directly so it works offline.
    match repo::get_album_art(&state.pool, &album_id).await {
        Ok(Some(row)) => {
            let path = PathBuf::from(&row.local_cover_path);
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    let mime = guess_mime(&path);
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, mime)
                        .header(header::CONTENT_LENGTH, bytes.len().to_string())
                        .header(header::CACHE_CONTROL, "max-age=3600")
                        .body(bytes)
                        .unwrap();
                }
                Err(e) => {
                    // Row exists but file is gone — fall through to the
                    // server proxy rather than 404 outright.
                    tracing::warn!(path = %path.display(), err = %e, "local cover file missing");
                }
            }
        }
        Ok(None) => { /* no local copy — try the server proxy */ }
        Err(e) => {
            tracing::warn!(err = %e, album_id = %album_id, "cover lookup failed");
            // fall through to the server proxy
        }
    }

    // 2. Server proxy — fetch the auth-gated server cover with the
    //    credential injected. Keeps the token out of the webview.
    let auth = state.auth.read().await.clone();
    let Some(auth): Option<Arc<AuthManager>> = auth else {
        return text(StatusCode::NOT_FOUND, "no local cover and not authenticated");
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return text(StatusCode::NOT_FOUND, "no local cover and no credential"),
    };
    match proxy_server_cover(&auth.server_config(), &cred, &album_id).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(err = %e, album_id = %album_id, "server cover proxy failed");
            text(StatusCode::NOT_FOUND, "no cover available")
        }
    }
}

/// One shared HTTP client for cover proxying (same rationale as the stream
/// proxy: avoid rebuilding the TLS session per request).
fn proxy_client() -> crate::error::AppResult<&'static reqwest::Client> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| crate::error::AppError::Transport(format!("cover proxy client: {e}")))?;
    Ok(CLIENT.get_or_init(|| c))
}

async fn proxy_server_cover(
    config: &crate::transport::ServerConfig,
    cred: &crate::transport::Credential,
    album_id: &str,
) -> crate::error::AppResult<Response<Vec<u8>>> {
    let client = proxy_client()?;
    let url = format!("{}/albums/{}/cover", config.rest_root(), album_id);
    let resp = client
        .get(&url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?)
        .send()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("cover proxy send: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Ok(text(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::NOT_FOUND),
            "no cover available",
        ));
    }

    let mut builder = Response::builder().status(StatusCode::OK);
    if let Some(v) = resp.headers().get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, v.clone());
    }
    builder = builder.header(header::CACHE_CONTROL, "max-age=3600");
    let body = resp
        .bytes()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("cover proxy body: {e}")))?;
    Ok(builder.body(body.to_vec()).unwrap())
}

fn auth_header_value(
    cred: &crate::transport::Credential,
) -> crate::error::AppResult<HeaderValue> {
    let s = match cred {
        crate::transport::Credential::SecretKey(k) => format!("SecretKey {k}"),
        crate::transport::Credential::Bearer(t) => format!("Bearer {t}"),
    };
    HeaderValue::from_str(&s).map_err(|_| crate::error::AppError::Internal("bad auth chars".into()))
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
