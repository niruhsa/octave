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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    let id = match parse_id(request.uri().path()) {
        Some(id) => id,
        None => return text(StatusCode::BAD_REQUEST, "missing id"),
    };
    // `?lowres=1` → serve the tiny placeholder variant. The cache key is the
    // full path+query (so it varies by id, lowres, and the `?v=` cache-bust).
    let lowres = wants_lowres(request);
    let key = cache_key(request);

    // `cover://localhost/artist/<id>` → artist image (server proxy only; the
    // offline cache doesn't store artist images). `cover://localhost/<id>` →
    // album cover (local-then-proxy, as before).
    if let Some(artist_id) = id.strip_prefix("artist/") {
        return dispatch_artist_image(app, artist_id, lowres, &key).await;
    }
    let album_id = id;

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
    let url = format!(
        "{}/albums/{}/cover{}",
        auth.server_config().rest_root(),
        album_id,
        lowres_query(lowres)
    );
    proxy_cached(&key, &cred, &url).await
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
        .user_agent(crate::USER_AGENT)
        .build()
        .map_err(|e| crate::error::AppError::Transport(format!("cover proxy client: {e}")))?;
    Ok(CLIENT.get_or_init(|| c))
}

/// Artist image: no offline cache, so always proxy `GET /artists/:id/image`
/// (through the in-memory image cache) with the session credential injected.
async fn dispatch_artist_image<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    artist_id: &str,
    lowres: bool,
    key: &str,
) -> Response<Vec<u8>> {
    let state = app.state::<AppStateHandle>();
    let auth = state.auth.read().await.clone();
    let Some(auth): Option<Arc<AuthManager>> = auth else {
        return text(StatusCode::NOT_FOUND, "not authenticated");
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return text(StatusCode::NOT_FOUND, "no credential"),
    };
    let url = format!(
        "{}/artists/{}/image{}",
        auth.server_config().rest_root(),
        artist_id,
        lowres_query(lowres)
    );
    proxy_cached(key, &cred, &url).await
}

// ── in-memory image cache ────────────────────────────────────────────────────
//
// Custom-scheme responses aren't reliably cached by the WebView (e.g. WKWebView
// re-invokes the protocol handler per `<img>` load), so without this every
// render of an online (non-downloaded) cover re-hit the server — the "takes a
// second or two every time" problem. This bounded LRU-ish byte cache makes a
// repeat render instant. Keyed by the full request path+query, so the `?v=`
// cache-bust + `?lowres=1` naturally partition entries; a 6 h TTL bounds
// staleness for pages that don't carry a `?v=`.

struct CachedImage {
    body: Arc<Vec<u8>>,
    content_type: Option<String>,
    seq: u64,
    at: Instant,
}

struct ImageCache {
    inner: Mutex<CacheInner>,
    max_bytes: usize,
    ttl: Duration,
}

#[derive(Default)]
struct CacheInner {
    map: HashMap<String, CachedImage>,
    bytes: usize,
    next_seq: u64,
}

impl ImageCache {
    fn get(&self, key: &str) -> Option<(Arc<Vec<u8>>, Option<String>)> {
        let mut g = self.inner.lock().unwrap();
        match g.map.get(key) {
            Some(e) if e.at.elapsed() <= self.ttl => Some((e.body.clone(), e.content_type.clone())),
            Some(_) => {
                // Stale — drop it.
                if let Some(e) = g.map.remove(key) {
                    g.bytes = g.bytes.saturating_sub(e.body.len());
                }
                None
            }
            None => None,
        }
    }

    fn put(&self, key: String, body: Arc<Vec<u8>>, content_type: Option<String>) {
        let len = body.len();
        if len == 0 || len > self.max_bytes {
            return; // don't cache empties or a single item bigger than the whole cache
        }
        let mut g = self.inner.lock().unwrap();
        let seq = g.next_seq;
        g.next_seq += 1;
        if let Some(old) = g.map.insert(key, CachedImage { body, content_type, seq, at: Instant::now() }) {
            g.bytes = g.bytes.saturating_sub(old.body.len());
        }
        g.bytes += len;
        // Evict the oldest entries (lowest seq) until back under the cap.
        while g.bytes > self.max_bytes && g.map.len() > 1 {
            let Some(oldest) = g.map.iter().min_by_key(|(_, e)| e.seq).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(e) = g.map.remove(&oldest) {
                g.bytes = g.bytes.saturating_sub(e.body.len());
            }
        }
    }
}

fn image_cache() -> &'static ImageCache {
    static CACHE: OnceLock<ImageCache> = OnceLock::new();
    CACHE.get_or_init(|| ImageCache {
        inner: Mutex::new(CacheInner::default()),
        max_bytes: 48 * 1024 * 1024, // 48 MiB — hundreds of covers + their low-res
        ttl: Duration::from_secs(6 * 3600),
    })
}

fn wants_lowres(request: &Request<Vec<u8>>) -> bool {
    request
        .uri()
        .query()
        .map(|q| q.split('&').any(|p| p == "lowres=1" || p == "lowres"))
        .unwrap_or(false)
}

fn lowres_query(lowres: bool) -> &'static str {
    if lowres {
        "?lowres=1"
    } else {
        ""
    }
}

/// Cache key = request path + query (id + lowres + `?v=` cache-bust).
fn cache_key(request: &Request<Vec<u8>>) -> String {
    match request.uri().query() {
        Some(q) => format!("{}?{}", request.uri().path(), q),
        None => request.uri().path().to_string(),
    }
}

/// Serve `url` for `key`, hitting the in-memory cache first. On a miss the
/// proxied bytes are cached. A non-success upstream status is relayed so a 404
/// stays a 404 and the `<img>` falls back to its placeholder.
async fn proxy_cached(
    key: &str,
    cred: &crate::transport::Credential,
    url: &str,
) -> Response<Vec<u8>> {
    if let Some((body, ct)) = image_cache().get(key) {
        return build_image_response(ct.as_deref(), (*body).clone());
    }
    match fetch_image(cred, url).await {
        Ok((status, ct, body)) if status.is_success() => {
            image_cache().put(key.to_string(), Arc::new(body.clone()), ct.clone());
            build_image_response(ct.as_deref(), body)
        }
        Ok((status, _, _)) => text(status, "no image available"),
        Err(e) => {
            tracing::warn!(err = %e, url, "image proxy failed");
            text(StatusCode::NOT_FOUND, "no image available")
        }
    }
}

/// Auth-inject + GET an image from the server. Returns `(status, content_type,
/// body)`; a non-success status carries an empty body.
async fn fetch_image(
    cred: &crate::transport::Credential,
    url: &str,
) -> crate::error::AppResult<(StatusCode, Option<String>, Vec<u8>)> {
    let client = proxy_client()?;
    let resp = client
        .get(url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?)
        .send()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("image proxy send: {e}")))?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !resp.status().is_success() {
        return Ok((status, None, Vec::new()));
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp
        .bytes()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("image proxy body: {e}")))?
        .to_vec();
    Ok((status, content_type, body))
}

fn build_image_response(content_type: Option<&str>, body: Vec<u8>) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, "max-age=3600")
        .header(header::CONTENT_LENGTH, body.len().to_string());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder.body(body).unwrap()
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

fn parse_id(path: &str) -> Option<String> {
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
