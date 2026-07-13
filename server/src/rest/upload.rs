//! Uploads v2 — REST transport (fallback to the gRPC primary).
//!
//! All endpoints delegate to the shared [`UploadsService`] so REST and gRPC
//! stay at exact feature parity. Routes:
//!   * `POST /uploads/init` — declare a session (list of files + chunk maps).
//!   * `POST /uploads/:id/files/:file_index/chunks/:chunk_index` — one chunk
//!     (raw body); the server verifies size + content hash.
//!   * `GET  /uploads` — list reports (`?user_id=`, `?state=`, `?limit=`, `?offset=`).
//!   * `GET  /uploads/:id` — one report (per-file/per-chunk detail).
//!   * `POST /uploads/:id/cancel` — cancel + clean staged chunks off disk.
//!   * `GET  /uploads/stream` — WebSocket of live progress (channel `uploads`).

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Extension, Path as AxPath, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::auth::Identity;
use crate::auth::service::Credential;
use crate::db::models::UploadState;
use crate::error::AppError;
use crate::rest::{ApiError, RestState, extract_credential};
use crate::services::{ChunkAck, FileInit, UploadHub, UploadView, UploadsService, can_see};
use crate::shutdown::ShutdownRx;

/// Per-chunk body ceiling (64 MiB) — also covers the largest plausible init
/// body (many files × chunk-hash lists). Clients pick a far smaller chunk size.
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

/// Authenticated `/uploads/*` routes (Identity injected by `auth_middleware`).
pub fn router() -> Router<RestState> {
    Router::new()
        .route("/uploads/init", post(init))
        .route(
            "/uploads/:id/files/:file_index/chunks/:chunk_index",
            post(chunk),
        )
        .route("/uploads", get(list))
        .route("/uploads/:id", get(get_one))
        .route("/uploads/:id/cancel", post(cancel))
        .route("/uploads/:id/pause", post(pause))
        .route("/uploads/:id/resume", post(resume))
        .route_layer(DefaultBodyLimit::max(MAX_CHUNK_BYTES))
}

/// The live-updates WebSocket. Mounted on the *public* router because the
/// browser WebSocket API can't set an `Authorization` header — this handler
/// authenticates from the header **or** a `?token=` / `?secret_key=` query
/// param itself.
pub fn ws_router() -> Router<RestState> {
    Router::new().route("/uploads/stream", get(stream))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn svc(state: &RestState) -> Result<&UploadsService, AppError> {
    state
        .uploads
        .as_ref()
        .ok_or_else(|| AppError::Config("uploads service not configured".into()))
}

fn parse_uuid(s: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(s).map_err(|e| AppError::InvalidArgument(format!("invalid upload id: {e}")))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct InitBody {
    files: Vec<FileInit>,
}

async fn init(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<InitBody>,
) -> Result<Json<UploadView>, ApiError> {
    let view = svc(&state)?.init(&caller, body.files).await?;
    Ok(Json(view))
}

async fn chunk(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    AxPath((id, file_index, chunk_index)): AxPath<(String, i32, i32)>,
    body: Bytes,
) -> Result<Json<ChunkAck>, ApiError> {
    let upload_id = parse_uuid(&id)?;
    let ack = svc(&state)?
        .put_chunk(&caller, upload_id, file_index, chunk_index, &body)
        .await?;
    Ok(Json(ack))
}

#[derive(Deserialize)]
struct ListQuery {
    user_id: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user = match q.user_id.as_deref() {
        Some(s) => Some(parse_uuid(s)?),
        None => None,
    };
    let st = match q.state.as_deref() {
        Some(s) => Some(
            UploadState::parse(s)
                .ok_or_else(|| AppError::InvalidArgument(format!("invalid state: {s}")))?,
        ),
        None => None,
    };
    let rows = svc(&state)?
        .list(
            &caller,
            user,
            st,
            q.limit.unwrap_or(50),
            q.offset.unwrap_or(0),
        )
        .await?;
    Ok(Json(json!({ "uploads": rows })))
}

async fn get_one(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    AxPath(id): AxPath<String>,
) -> Result<Json<UploadView>, ApiError> {
    let view = svc(&state)?.get(&caller, parse_uuid(&id)?).await?;
    Ok(Json(view))
}

async fn cancel(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    AxPath(id): AxPath<String>,
) -> Result<Json<UploadView>, ApiError> {
    let view = svc(&state)?.cancel(&caller, parse_uuid(&id)?).await?;
    Ok(Json(view))
}

async fn pause(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    AxPath(id): AxPath<String>,
) -> Result<Json<UploadView>, ApiError> {
    let view = svc(&state)?.pause(&caller, parse_uuid(&id)?).await?;
    Ok(Json(view))
}

async fn resume(
    State(state): State<RestState>,
    Extension(caller): Extension<Identity>,
    AxPath(id): AxPath<String>,
) -> Result<Json<UploadView>, ApiError> {
    let view = svc(&state)?.resume(&caller, parse_uuid(&id)?).await?;
    Ok(Json(view))
}

// ---------------------------------------------------------------------------
// WebSocket: live `uploads` channel
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StreamQuery {
    token: Option<String>,
    secret_key: Option<String>,
}

async fn stream(
    State(state): State<RestState>,
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    // Credential from the Authorization header (native clients) or query params
    // (browsers, which can't set WS headers).
    let cred = extract_credential(&headers)
        .or_else(|| q.token.map(Credential::Bearer))
        .or_else(|| q.secret_key.map(Credential::SecretKey))
        .ok_or_else(|| AppError::Unauthenticated("missing credential for uploads stream".into()))?;
    let identity = state.auth.resolve(cred).await?;
    let hub = state.upload_hub.clone();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| stream_loop(socket, identity, hub, shutdown)))
}

/// Forward permitted upload events to one subscriber until either side closes
/// or the server shuts down.
async fn stream_loop(
    mut socket: WebSocket,
    identity: Identity,
    hub: UploadHub,
    mut shutdown: ShutdownRx,
) {
    let mut rx = hub.subscribe();
    loop {
        tokio::select! {
            // Server is shutting down — close so the graceful drain can finish.
            _ = shutdown.changed() => break,
            event = rx.recv() => match event {
                Ok(ev) => {
                    if can_see(&identity, ev.owner_id) {
                        match serde_json::to_string(&ev) {
                            Ok(txt) => {
                                if socket.send(Message::Text(txt)).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => continue, // dropped some progress; keep going
                Err(RecvError::Closed) => break,
            },
            // Drain inbound frames so we notice the client closing / pings.
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}        // ignore client chatter
                Some(Err(_)) => break,
            },
        }
    }
}
