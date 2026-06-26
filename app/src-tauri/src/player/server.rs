//! In-app loopback HTTP server for media playback.
//!
//! ## Why this exists (and replaced the old `media://` custom protocol)
//!
//! Tauri/wry custom protocols can't stream: the responder takes a fully
//! buffered `Vec<u8>` body. On desktop we worked around that by serving a
//! windowed `206`, but Android made it impossible — the WebView neither
//! forwards `Range` to intercepted requests (so we can't window) nor tolerates
//! the ~30 s budget wry gives the handler to produce the whole buffered body
//! (so we can't fall back to the full file for a large track). Playback either
//! replayed the first window forever or timed out with `MEDIA_ERR_NETWORK`.
//!
//! A real HTTP origin sidesteps all of it. Android's media stack talks normal
//! HTTP to `http://127.0.0.1:<port>` — real `Range` requests, `206`
//! continuation, progressive buffering — and we stream the body straight
//! through (`reqwest::Response::bytes_stream` → [`axum`] `Body::from_stream`)
//! with no buffering and no timeout. Fast start *and* it plays through, on
//! every platform, from one code path.
//!
//! ## Security
//!
//! The listener binds `127.0.0.1:0` (loopback only). On Android other apps on
//! the device can still reach loopback ports, so every URL carries a random
//! per-launch token (`/s/<token>/<id>`); requests with the wrong token get
//! `403`. The token only ever appears in local URLs handed to the webview — it
//! never crosses the network.
//!
//! ## Per request
//!
//! * **Local file** — if the track id is in the SQLite cache (downloaded),
//!   stream `local_file_path` from disk with full RFC 7233 range semantics
//!   (`200` / `206` / `416`).
//! * **Server stream** — else proxy `GET /tracks/{id}/stream`, injecting the
//!   `Authorization` header and forwarding `Range`, and relay the server's
//!   streamed body + status + headers.
//! * **Offline + not cached** — `502`, so the `<audio>` element fires `error`
//!   and the UI can surface "not available offline".

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tauri::{Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::AppStateHandle;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::transport::{Credential, ServerConfig};

/// Loopback bind + token, published to the frontend via `player_media_url`.
pub struct MediaServer {
    pub port: u16,
    pub token: String,
}

#[derive(Clone)]
struct ServerState {
    app: tauri::AppHandle,
    token: Arc<str>,
}

/// Build the axum router for the media server.
pub fn router(app: tauri::AppHandle, token: Arc<str>) -> Router {
    Router::new()
        .route("/s/:token/:id", get(serve).head(serve))
        .route("/cover/:token/:id", get(serve_cover).head(serve_cover))
        .route("/action/:token/:action", get(serve_action))
        .with_state(ServerState { app, token })
}

/// The loopback URL the webview's `<audio>` element loads for `track_id`.
pub fn media_url(port: u16, token: &str, track_id: &str) -> String {
    format!("http://127.0.0.1:{port}/s/{token}/{}", encode_segment(track_id))
}

/// Like [`media_url`] but for a podcast **episode** id — adds `?kind=episode`
/// so the server resolves it against the episode cache + the
/// `/podcasts/episodes/{id}/stream` endpoint instead of the track ones.
pub fn episode_media_url(port: u16, token: &str, episode_id: &str) -> String {
    format!(
        "http://127.0.0.1:{port}/s/{token}/{}?kind=episode",
        encode_segment(episode_id)
    )
}

/// The loopback URL native code fetches album art from. The webview uses the
/// `cover://` scheme (see `assets`), but the Android media-notification service
/// is native Kotlin and can only reach a real HTTP origin — so it loads the
/// cover from this loopback route, which resolves it the same way.
pub fn cover_url(port: u16, token: &str, album_id: &str) -> String {
    format!("http://127.0.0.1:{port}/cover/{token}/{}", encode_segment(album_id))
}

/// Base URL the native media-session code hits to deliver a transport-button
/// press back to the app (`<base>/<action>` → a `media-session-action` Tauri
/// event the frontend listens for). Native Kotlin can't emit a Tauri event or
/// reach the plugin event channel (ACL), so it routes through this loopback
/// route instead.
pub fn action_base_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/action/{token}")
}

#[derive(serde::Deserialize)]
struct ActionQuery {
    /// Target position (ms) for the `seek` action.
    pos: Option<i64>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaActionEvent {
    action: String,
    position_ms: Option<i64>,
}

/// Receive a transport action from the native media notification / lock screen
/// and re-broadcast it as a `media-session-action` Tauri event for the player
/// store to act on.
async fn serve_action(
    State(st): State<ServerState>,
    Path((token, action)): Path<(String, String)>,
    Query(q): Query<ActionQuery>,
) -> Response {
    if token.as_bytes() != st.token.as_bytes() {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let _ = st.app.emit(
        "media-session-action",
        MediaActionEvent {
            action,
            position_ms: q.pos,
        },
    );
    (StatusCode::OK, "ok").into_response()
}

#[derive(serde::Deserialize)]
struct ServeQuery {
    /// `"episode"` to resolve a podcast episode; absent/anything else = track.
    kind: Option<String>,
}

async fn serve(
    State(st): State<ServerState>,
    Path((token, id)): Path<(String, String)>,
    Query(q): Query<ServeQuery>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    // Constant work regardless; the token is short and local, so a plain
    // compare is fine (this gates other local apps, not network attackers).
    if token.as_bytes() != st.token.as_bytes() {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }

    // State is managed during `setup`, before the webview can issue a request,
    // but guard anyway so a stray early hit can't panic the server task.
    let Some(state) = st.app.try_state::<AppStateHandle>() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "starting up").into_response();
    };

    // Podcast episode: resolve against the episode cache + episode stream
    // endpoint. (Non-downloaded, non-server-cached episodes are played from
    // their origin enclosure URL directly by the frontend, so this only needs
    // the local + server-proxy paths.)
    if q.kind.as_deref() == Some("episode") {
        return serve_episode(&state, &id, &headers, &method).await;
    }

    // 1. Local cache hit → stream the downloaded file.
    match repo::get_track(&state.pool, &id).await {
        Ok(Some(row)) => return serve_local(&row.local_file_path, &headers, &method).await,
        Ok(None) => { /* fall through to the server stream */ }
        Err(e) => tracing::warn!(err = %e, track = %id, "cache lookup failed; trying server"),
    }

    // 1b. Look-ahead prefetch hit → serve the temp file like a local one. This
    // is what lets a *streamed* track advance with the screen off: the next
    // track was fetched to disk while the previous one played, so the webview
    // loads a local file (never suspended) instead of a network stream (which
    // the hidden WebView won't start). See `super::prefetch`.
    if let Some(cache) = st.app.try_state::<crate::player::prefetch::PrefetchCache>() {
        if let Some((path, content_type)) = cache.ready(&id) {
            if let Some(p) = path.to_str() {
                return serve_local_typed(p, &content_type, &headers, &method).await;
            }
        }
    }

    // 2. Server stream — needs an auth manager + credential.
    let auth = state.auth.read().await.clone();
    let Some(auth) = auth else {
        return (StatusCode::UNAUTHORIZED, "not configured — log in first").into_response();
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "no active session").into_response(),
    };
    let track_url = format!("{}/tracks/{}/stream", auth.server_config().rest_root(), id);
    match proxy_stream(&track_url, &cred, &headers, &method).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(err = %e, track = %id, "server stream failed");
            // 502: couldn't reach the authority and the track isn't cached.
            // Distinct from 404 so the UI tells "offline, not downloaded" apart
            // from "track missing".
            (
                StatusCode::BAD_GATEWAY,
                "stream unavailable (offline and not downloaded)",
            )
                .into_response()
        }
    }
}

/// Resolve a podcast episode: a downloaded local file, else proxy the server's
/// episode stream endpoint. Mirrors the track path in [`serve`].
async fn serve_episode(
    state: &AppStateHandle,
    id: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    // 1. Local cache hit → downloaded file.
    match repo::get_episode(&state.pool, id).await {
        Ok(Some(row)) => {
            if let Some(lp) = row.local_file_path.as_deref() {
                return serve_local(lp, headers, method).await;
            }
        }
        Ok(None) => { /* fall through to the server stream */ }
        Err(e) => tracing::warn!(err = %e, episode = %id, "episode cache lookup failed"),
    }

    // 2. Proxy the server's episode stream endpoint (the server has it on disk).
    let auth = state.auth.read().await.clone();
    let Some(auth) = auth else {
        return (StatusCode::UNAUTHORIZED, "not configured — log in first").into_response();
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return (StatusCode::UNAUTHORIZED, "no active session").into_response(),
    };
    let url = format!(
        "{}/podcasts/episodes/{}/stream",
        auth.server_config().rest_root(),
        id
    );
    match proxy_stream(&url, &cred, headers, method).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(err = %e, episode = %id, "episode stream failed");
            (
                StatusCode::BAD_GATEWAY,
                "stream unavailable (offline and not downloaded)",
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Cover art (for the native media-session notification)
// ---------------------------------------------------------------------------

/// Serve album art over the loopback origin: a downloaded local cover, else the
/// auth-proxied server cover (`GET /albums/{id}/cover`). Mirrors `assets`'s
/// `cover://` resolution but as real HTTP so native Kotlin can fetch the bitmap.
async fn serve_cover(
    State(st): State<ServerState>,
    Path((token, id)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if token.as_bytes() != st.token.as_bytes() {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let Some(state) = st.app.try_state::<AppStateHandle>() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "starting up").into_response();
    };

    // 1. Local downloaded cover — serve directly (works offline).
    if let Ok(Some(row)) = repo::get_album_art(&state.pool, &id).await {
        if tokio::fs::metadata(&row.local_cover_path).await.is_ok() {
            return serve_local(&row.local_cover_path, &headers, &method).await;
        }
    }

    // 2. Auth-proxied server cover.
    let auth = state.auth.read().await.clone();
    let Some(auth) = auth else {
        return (StatusCode::NOT_FOUND, "no cover").into_response();
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => return (StatusCode::NOT_FOUND, "no cover").into_response(),
    };
    match proxy_cover(&auth.server_config(), &cred, &id).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(err = %e, album = %id, "cover proxy failed");
            (StatusCode::NOT_FOUND, "no cover").into_response()
        }
    }
}

async fn proxy_cover(
    config: &ServerConfig,
    cred: &Credential,
    album_id: &str,
) -> AppResult<Response> {
    let client = proxy_client()?;
    let url = format!("{}/albums/{}/cover", config.rest_root(), album_id);
    let resp = client
        .get(&url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?)
        .send()
        .await
        .map_err(|e| AppError::Transport(format!("cover proxy send: {e}")))?;
    if !resp.status().is_success() {
        return Ok((StatusCode::NOT_FOUND, "no cover").into_response());
    }
    let mut builder = Response::builder().status(StatusCode::OK);
    if let Some(v) = resp.headers().get(header::CONTENT_TYPE) {
        builder = builder.header(header::CONTENT_TYPE, v.clone());
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::Transport(format!("cover proxy body: {e}")))?;
    builder
        .body(Body::from(body))
        .map_err(|e| AppError::Internal(format!("build cover response: {e}")))
}

// ---------------------------------------------------------------------------
// Local file serving
// ---------------------------------------------------------------------------

async fn serve_local(path: &str, headers: &HeaderMap, method: &Method) -> Response {
    serve_local_typed(path, guess_mime(&PathBuf::from(path)), headers, method).await
}

/// Like [`serve_local`] but with an explicit Content-Type — used for prefetched
/// temp files, which carry no extension for [`guess_mime`] to read.
async fn serve_local_typed(
    path: &str,
    content_type: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    let path = PathBuf::from(path);
    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), err = %e, "local media file missing");
            return (StatusCode::NOT_FOUND, "local file not found").into_response();
        }
    };
    if !meta.is_file() {
        return (StatusCode::NOT_FOUND, "not a file").into_response();
    }
    let total = meta.len();

    // Resolve the byte window. No `Range` (or a malformed one, per RFC 7233
    // §3.1) → the whole file as `200`; a satisfiable range → `206`; a parsed
    // but out-of-bounds range → `416`.
    let (status, start, end) = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
        Some(h) => match parse_range(h, total) {
            Rng::Sat(s, e) => (StatusCode::PARTIAL_CONTENT, s, e),
            Rng::Malformed => (StatusCode::OK, 0, total.saturating_sub(1)),
            Rng::Unsatisfiable => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::empty())
                    .unwrap();
            }
        },
    };

    let len = if total == 0 { 0 } else { end - start + 1 };

    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len.to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"));
    }

    // HEAD (and the empty-file edge case) return headers only.
    if method == Method::HEAD || total == 0 {
        return builder.body(Body::empty()).unwrap();
    }

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("open: {e}")).into_response(),
    };
    if start > 0 {
        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("seek: {e}")).into_response();
        }
    }
    // `take` bounds the body at the reader layer; `ReaderStream` reads in 4 KiB
    // chunks so we never hold the whole file in memory.
    let stream = ReaderStream::new(file.take(len));
    builder.body(Body::from_stream(stream)).unwrap()
}

/// Single-range `Range` parse against a known total. Mirrors the server's
/// `rest::range` (multi-range is rejected as malformed → whole body).
enum Rng {
    Sat(u64, u64),
    Unsatisfiable,
    Malformed,
}

fn parse_range(header: &str, total: u64) -> Rng {
    let spec = match header.trim().strip_prefix("bytes=") {
        Some(s) => s.trim(),
        None => return Rng::Malformed,
    };
    if spec.is_empty() || spec.contains(',') {
        return Rng::Malformed;
    }
    let (lhs, rhs) = match spec.split_once('-') {
        Some(x) => x,
        None => return Rng::Malformed,
    };
    let (lhs, rhs) = (lhs.trim(), rhs.trim());
    match (lhs.is_empty(), rhs.is_empty()) {
        // bytes=-N — last N bytes.
        (true, false) => {
            let n: u64 = match rhs.parse() {
                Ok(n) => n,
                Err(_) => return Rng::Malformed,
            };
            if n == 0 || total == 0 {
                return Rng::Unsatisfiable;
            }
            let n = n.min(total);
            Rng::Sat(total - n, total - 1)
        }
        // bytes=N- — N to EOF.
        (false, true) => {
            let start: u64 = match lhs.parse() {
                Ok(s) => s,
                Err(_) => return Rng::Malformed,
            };
            if start >= total {
                return Rng::Unsatisfiable;
            }
            Rng::Sat(start, total - 1)
        }
        // bytes=A-B — explicit window (B clipped to EOF).
        (false, false) => {
            let start: u64 = match lhs.parse() {
                Ok(s) => s,
                Err(_) => return Rng::Malformed,
            };
            let end: u64 = match rhs.parse() {
                Ok(e) => e,
                Err(_) => return Rng::Malformed,
            };
            if start > end || start >= total {
                return Rng::Unsatisfiable;
            }
            Rng::Sat(start, end.min(total - 1))
        }
        (true, true) => Rng::Malformed,
    }
}

// ---------------------------------------------------------------------------
// Server stream proxy
// ---------------------------------------------------------------------------

/// One shared HTTP client for all proxying. Rebuilding a client per range
/// request (the `<audio>` element issues many) would thrash the TLS session
/// cache and add latency — audible as stutter on seek.
///
/// The timeouts matter for background (screen-off) playback. The `<audio>`
/// element buffers a track ahead, so its proxy connection then sits idle for
/// minutes; when the radio sleeps that socket dies silently. Without a short
/// idle timeout the *next* track would pick that dead connection out of the pool
/// and hang forever (no error, playback just stops) — so we prune idle
/// connections aggressively and bound `connect`, turning a would-be hang into a
/// fast fresh connection (or a clean `error` the UI can show). No overall
/// request timeout: streaming a whole track legitimately takes minutes.
pub(crate) fn proxy_client() -> AppResult<&'static reqwest::Client> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let c = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::Transport(format!("proxy client: {e}")))?;
    Ok(CLIENT.get_or_init(|| c))
}

async fn proxy_stream(
    url: &str,
    cred: &Credential,
    headers: &HeaderMap,
    method: &Method,
) -> AppResult<Response> {
    let client = proxy_client()?;

    let mut req = client
        .request(method.clone(), url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?);
    // Forward the Range verbatim — a real HTTP server, so no windowing needed.
    if let Some(range) = headers.get(header::RANGE) {
        req = req.header(header::RANGE, range.clone());
    }

    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Transport(format!("proxy send: {e}")))?;

    let status = StatusCode::from_u16(resp.status().as_u16())
        .map_err(|e| AppError::Internal(format!("bad upstream status: {e}")))?;
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

    // Stream the body straight through — never buffer the whole track.
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .map_err(|e| AppError::Internal(format!("build response: {e}")))
}

pub(crate) fn auth_header_value(cred: &Credential) -> AppResult<axum::http::HeaderValue> {
    let s = match cred {
        Credential::SecretKey(k) => format!("SecretKey {k}"),
        Credential::Bearer(t) => format!("Bearer {t}"),
    };
    axum::http::HeaderValue::from_str(&s).map_err(|_| AppError::Internal("bad auth chars".into()))
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Conservative extension→MIME map. Anything unknown streams as
/// `application/octet-stream` so the player decides what to do.
fn guess_mime(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg" | "oga") => "audio/ogg",
        Some("opus") => "audio/opus",
        Some("m4a" | "mp4" | "aac") => "audio/mp4",
        Some("webm") => "audio/webm",
        _ => "application/octet-stream",
    }
}

/// Percent-encode the bytes of a path segment that aren't URL-safe. Server
/// UUIDs are already safe; this just defends against a stray `/` or space.
fn encode_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_url_shape() {
        let u = media_url(50505, "tok123", "01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(
            u,
            "http://127.0.0.1:50505/s/tok123/01234567-89ab-cdef-0123-456789abcdef"
        );
    }

    #[test]
    fn encode_segment_escapes_unsafe() {
        assert_eq!(encode_segment("abc-123"), "abc-123");
        assert_eq!(encode_segment("a/b"), "a%2Fb");
        assert_eq!(encode_segment("a b"), "a%20b");
    }

    #[test]
    fn range_explicit_and_open() {
        assert!(matches!(parse_range("bytes=0-99", 1000), Rng::Sat(0, 99)));
        assert!(matches!(parse_range("bytes=500-", 1000), Rng::Sat(500, 999)));
        // upper bound clipped to EOF
        assert!(matches!(parse_range("bytes=0-99999", 1000), Rng::Sat(0, 999)));
    }

    #[test]
    fn range_suffix() {
        assert!(matches!(parse_range("bytes=-200", 1000), Rng::Sat(800, 999)));
        assert!(matches!(parse_range("bytes=-5000", 1000), Rng::Sat(0, 999)));
    }

    #[test]
    fn range_unsatisfiable_and_malformed() {
        assert!(matches!(parse_range("bytes=1000-1100", 1000), Rng::Unsatisfiable));
        assert!(matches!(parse_range("bytes=-0", 1000), Rng::Unsatisfiable));
        assert!(matches!(parse_range("octets=0-1", 1000), Rng::Malformed));
        assert!(matches!(parse_range("bytes=0-9,20-29", 1000), Rng::Malformed));
    }

    // ---- local-file serving (the seek + take + ReaderStream path) ----

    async fn body_bytes(resp: Response) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    async fn temp_with(name: &str, data: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        tokio::fs::write(&p, data).await.unwrap();
        (dir, p)
    }

    #[tokio::test]
    async fn local_full_file_is_200_whole_body() {
        let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let (_d, p) = temp_with("a.mp3", &data).await;

        let resp = serve_local(p.to_str().unwrap(), &HeaderMap::new(), &Method::GET).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "10000");
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "audio/mpeg");
        assert_eq!(body_bytes(resp).await, data);
    }

    #[tokio::test]
    async fn local_range_is_206_with_exact_slice() {
        let data: Vec<u8> = (0..100u8).collect();
        let (_d, p) = temp_with("a.flac", &data).await;

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=10-19".parse().unwrap());
        let resp = serve_local(p.to_str().unwrap(), &h, &Method::GET).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(), "bytes 10-19/100");
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "10");
        assert_eq!(body_bytes(resp).await, (10..20u8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn local_open_ended_range_streams_to_eof() {
        let data: Vec<u8> = (0..100u8).collect();
        let (_d, p) = temp_with("a.flac", &data).await;

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=90-".parse().unwrap());
        let resp = serve_local(p.to_str().unwrap(), &h, &Method::GET).await;
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(), "bytes 90-99/100");
        assert_eq!(body_bytes(resp).await, (90..100u8).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn local_head_has_headers_but_no_body() {
        let (_d, p) = temp_with("a.wav", &vec![7u8; 500]).await;

        let resp = serve_local(p.to_str().unwrap(), &HeaderMap::new(), &Method::HEAD).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "500");
        assert!(body_bytes(resp).await.is_empty());
    }

    #[tokio::test]
    async fn local_unsatisfiable_range_is_416() {
        let (_d, p) = temp_with("a.ogg", &vec![0u8; 50]).await;

        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=100-200".parse().unwrap());
        let resp = serve_local(p.to_str().unwrap(), &h, &Method::GET).await;
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(), "bytes */50");
    }

    #[tokio::test]
    async fn local_missing_file_is_404() {
        let resp = serve_local("/no/such/file.mp3", &HeaderMap::new(), &Method::GET).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
