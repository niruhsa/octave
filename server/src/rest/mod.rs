//! REST transport (fallback).
//!
//! All routes use the shared `AuthService` to gate access. `AppError` is
//! mapped into HTTP statuses by `ApiError`.

pub mod ingest;
pub mod library;
pub mod notification;
pub mod playlist;
pub mod range;
pub mod streaming;
pub mod upload;

use std::net::SocketAddr;

use axum::{
    Json, Router,
    body::Body,
    extract::Request,
    extract::State,
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::auth::Identity;
use crate::auth::service::{AuthService, Credential};
use crate::db::models::PermissionLevel;
use crate::error::{AppError, Result};
use crate::shutdown::{wait_for_shutdown, ShutdownRx};
use crate::services::{
    ArtworkService, ImageOptimizer, IngestService, LibraryService, MetadataService,
    NotificationService, PlaylistService, ScanService, StreamingService, UploadHub, UploadsService,
};

/// Shared state injected into every handler.
#[derive(Clone)]
pub struct RestState {
    pub auth: AuthService,
    pub library: LibraryService,
    pub scan: ScanService,
    pub streaming: StreamingService,
    pub playlists: PlaylistService,
    pub notifications: NotificationService,
    pub ingest: Option<IngestService>,
    pub metadata: MetadataService,
    pub artwork: Option<ArtworkService>,
    /// Image optimizer (None when no `ARTWORK_PATH` is configured). Serves
    /// downscaled cover/artist images, generating them on demand.
    pub optimizer: Option<ImageOptimizer>,
    /// DB-backed upload sessions (None when no ingest staging dir is configured).
    pub uploads: Option<UploadsService>,
    /// Live upload-progress broadcast hub (shared with the gRPC stream).
    pub upload_hub: UploadHub,
    /// Server shutdown flag — drives the graceful drain and lets the live
    /// uploads WebSocket close itself instead of blocking shutdown.
    pub shutdown: ShutdownRx,
}

/// Run the REST server until shutdown.
pub async fn serve(addr: SocketAddr, state: RestState) -> Result<()> {
    let public = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(login))
        // The uploads live-stream authenticates itself (header or query param),
        // so it sits outside the header-only `auth_middleware`.
        .merge(upload::ws_router());

    let protected = Router::new()
        .route("/auth/whoami", get(whoami))
        .route("/auth/register", post(register))
        .route("/auth/logout", post(logout))
        .route("/users", get(list_users))
        .route("/users/:id", delete(delete_user))
        .route("/users/:id/password", put(change_password))
        .merge(library::router())
        .merge(playlist::router())
        .merge(notification::router())
        .merge(streaming::router())
        .merge(ingest::router())
        .merge(upload::router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let shutdown = state.shutdown.clone();
    let app = public.merge(protected).with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AppError::Internal(format!("REST bind {addr} failed: {e}")))?;

    info!(%addr, "REST server listening");

    let shutdown_signal = async move {
        wait_for_shutdown(shutdown).await;
        info!("REST server received shutdown signal");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .map_err(|e| AppError::Internal(format!("REST server error: {e}")))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<RestState>,
    mut req: Request<Body>,
    next: Next,
) -> std::result::Result<Response, ApiError> {
    let cred = extract_credential(req.headers())
        .ok_or_else(|| AppError::Unauthenticated("missing Authorization header".into()))?;
    let identity = state.auth.resolve(cred).await?;
    req.extensions_mut().insert(identity);
    Ok(next.run(req).await)
}

pub(crate) fn extract_credential(headers: &HeaderMap) -> Option<Credential> {
    let raw = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    if let Some(raw) = raw {
        let raw = raw.trim();
        if let Some(rest) = strip_ci_prefix(raw, "Bearer ") {
            return Some(Credential::Bearer(rest.trim().to_string()));
        }
        if let Some(rest) = strip_ci_prefix(raw, "SecretKey ") {
            return Some(Credential::SecretKey(rest.trim().to_string()));
        }
    }
    if let Some(v) = headers.get("x-secret-key").and_then(|v| v.to_str().ok()) {
        return Some(Credential::SecretKey(v.trim().to_string()));
    }
    None
}

fn strip_ci_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Auth handlers
// ---------------------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct LoginResponse {
    token: String,
    user_id: String,
    level: PermissionLevel,
    expires_at: String,
}

async fn login(
    State(state): State<RestState>,
    Json(body): Json<LoginRequest>,
) -> std::result::Result<Json<LoginResponse>, ApiError> {
    let out = state.auth.login(&body.username, &body.password).await?;
    Ok(Json(LoginResponse {
        token: out.token,
        user_id: out.user_id.to_string(),
        level: out.level,
        expires_at: out.expires_at.to_string(),
    }))
}

#[derive(Serialize)]
struct WhoAmI {
    kind: &'static str,
    user_id: Option<String>,
    username: Option<String>,
    level: PermissionLevel,
}

async fn whoami(req: Request<Body>) -> std::result::Result<Json<WhoAmI>, ApiError> {
    let id = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?;
    Ok(Json(match id {
        Identity::SecretKey => WhoAmI {
            kind: "secret_key",
            user_id: None,
            username: None,
            level: PermissionLevel::Admin,
        },
        Identity::User {
            id,
            username,
            level,
        } => WhoAmI {
            kind: "user",
            user_id: Some(id.to_string()),
            username: Some(username.clone()),
            level: *level,
        },
    }))
}

#[derive(Deserialize)]
struct RegisterRequest {
    username: String,
    password: String,
    level: PermissionLevel,
}

async fn register(
    State(state): State<RestState>,
    req: Request<Body>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let caller = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?
        .clone();

    let body: RegisterRequest = parse_json(req).await?;
    let id = state
        .auth
        .register(&caller, &body.username, &body.password, body.level)
        .await?;
    Ok(Json(serde_json::json!({ "user_id": id.to_string() })))
}

async fn logout(
    State(state): State<RestState>,
    headers: HeaderMap,
) -> std::result::Result<StatusCode, ApiError> {
    if let Some(Credential::Bearer(t)) = extract_credential(&headers) {
        state.auth.logout(&t).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    // Empty/omitted for admin resets; required + verified for non-admin
    // self-changes.
    #[serde(default)]
    old_password: String,
    new_password: String,
}

/// `PUT /users/:id/password` — change (or admin-reset) a user's password.
/// Authorization mirrors the gRPC `ChangePassword` RPC: admin/secret-key
/// callers may reset any user (old_password ignored); non-admin callers
/// may only reset their own and must supply a verified `old_password`.
async fn change_password(
    State(state): State<RestState>,
    req: Request<Body>,
) -> std::result::Result<StatusCode, ApiError> {
    let caller = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?
        .clone();

    let id = req
        .uri()
        .path()
        .rsplit('/')
        .nth(1)
        .ok_or_else(|| AppError::InvalidArgument("missing user id in path".into()))?;
    let target_id = Uuid::parse_str(id)
        .map_err(|e| AppError::InvalidArgument(format!("invalid user id: {e}")))?;

    let body: ChangePasswordRequest = parse_json(req).await?;
    let old = if body.old_password.is_empty() {
        None
    } else {
        Some(body.old_password.as_str())
    };
    state
        .auth
        .change_password(&caller, target_id, old, &body.new_password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /users` — list every registered user (admin-gated). Returns
/// `[{id, username, level}]` — no password hashes. Used by the client
/// to populate the admin password-reset dropdown.
async fn list_users(
    State(state): State<RestState>,
    req: Request<Body>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let caller = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?
        .clone();
    let users = state.auth.list_users(&caller).await?;
    Ok(Json(serde_json::json!({
        "users": users
            .iter()
            .map(|u| serde_json::json!({
                "id": u.id.to_string(),
                "username": u.username,
                "level": u.level,
            }))
            .collect::<Vec<_>>()
    })))
}

/// `DELETE /users/:id` — delete a user (admin-gated). Cascades:
/// sessions, playlists, follows (`ON DELETE CASCADE`); audit-log
/// actor references become NULL. An audit `user.delete` entry is
/// written before the row is destroyed.
async fn delete_user(
    State(state): State<RestState>,
    req: Request<Body>,
) -> std::result::Result<StatusCode, ApiError> {
    let caller = req
        .extensions()
        .get::<Identity>()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()))?
        .clone();

    let id = req
        .uri()
        .path()
        .rsplit('/')
        .nth(1)
        .ok_or_else(|| AppError::InvalidArgument("missing user id in path".into()))?;
    let target_id = Uuid::parse_str(id)
        .map_err(|e| AppError::InvalidArgument(format!("invalid user id: {e}")))?;

    state.auth.delete_user(&caller, target_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub(crate) async fn parse_json<T: for<'de> Deserialize<'de>>(req: Request<Body>) -> Result<T> {
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
        .await
        .map_err(|e| AppError::InvalidArgument(format!("read body: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::InvalidArgument(format!("invalid JSON: {e}")))
}

pub struct ApiError(pub AppError);

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            AppError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            AppError::PermissionDenied(_) => StatusCode::FORBIDDEN,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::InvalidArgument(_) => StatusCode::BAD_REQUEST,
            AppError::Config(_) | AppError::Internal(_) | AppError::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, self.0.to_string()).into_response()
    }
}

