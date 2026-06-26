//! REST follows & notifications routes — feature parity with the gRPC
//! `NotificationService` (Phase 10).
//!
//! Route shapes avoid mixing a path param and a static segment at the same
//! position (a matchit 0.7 conflict): the per-notification action takes its id
//! in the JSON body (`/notifications/mark-read`) rather than the path, so every
//! segment under `/notifications` stays static.

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::rest::{ApiError, RestState};

pub fn router() -> Router<RestState> {
    Router::new()
        // Follows
        .route(
            "/artists/:id/follow",
            post(follow).delete(unfollow).get(is_following),
        )
        .route("/following", get(list_following))
        // Notifications
        .route("/notifications", get(list_notifications))
        .route("/notifications/unread-count", get(unread_count))
        .route("/notifications/mark-read", post(mark_read))
        .route("/notifications/mark-all-read", post(mark_all_read))
        // Device push tokens (FCM)
        .route("/devices", post(register_device).delete(unregister_device))
}

// ---------------------------------------------------------------------------
// Helpers / DTOs
// ---------------------------------------------------------------------------

fn id(req: &Request<Body>) -> Result<Identity, ApiError> {
    req.extensions()
        .get::<Identity>()
        .cloned()
        .ok_or_else(|| crate::error::AppError::Unauthenticated("missing identity".into()).into())
}

#[derive(Serialize)]
pub struct FollowedArtistDto {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub image_path: Option<String>,
}
fn artist_dto(a: m::Artist) -> FollowedArtistDto {
    FollowedArtistDto {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name,
        image_path: a.image_path,
    }
}

#[derive(Serialize)]
pub struct NotificationDto {
    pub id: String,
    pub kind: String,
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub read: bool,
    pub created_at: String,
}
fn notification_dto(n: m::Notification) -> NotificationDto {
    NotificationDto {
        id: n.id.to_string(),
        kind: n.kind,
        artist_id: n.artist_id.map(|id| id.to_string()),
        album_id: n.album_id.map(|id| id.to_string()),
        title: n.title,
        body: n.body,
        read: n.read_at.is_some(),
        created_at: n.created_at.to_string(),
    }
}

#[derive(Serialize)]
pub struct FollowStatusDto {
    pub following: bool,
}

#[derive(Serialize)]
pub struct ListFollowingDto {
    pub artists: Vec<FollowedArtistDto>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct ListNotificationsDto {
    pub notifications: Vec<NotificationDto>,
    pub total: i64,
    pub unread_count: i64,
}

// ---------------------------------------------------------------------------
// Follows
// ---------------------------------------------------------------------------

async fn follow(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<FollowStatusDto>, ApiError> {
    let caller = id(&req)?;
    state.notifications.follow(&caller, artist_id).await?;
    Ok(Json(FollowStatusDto { following: true }))
}

async fn unfollow(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    state.notifications.unfollow(&caller, artist_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn is_following(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<FollowStatusDto>, ApiError> {
    let caller = id(&req)?;
    let following = state.notifications.is_following(&caller, artist_id).await?;
    Ok(Json(FollowStatusDto { following }))
}

async fn list_following(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<ListFollowingDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.notifications.list_following(&caller).await?;
    let total = rows.len() as i64;
    Ok(Json(ListFollowingDto {
        artists: rows.into_iter().map(artist_dto).collect(),
        total,
    }))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NotificationsQuery {
    #[serde(default)]
    pub unread: bool,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

async fn list_notifications(
    State(state): State<RestState>,
    Query(q): Query<NotificationsQuery>,
    req: Request<Body>,
) -> Result<Json<ListNotificationsDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state
        .notifications
        .list_notifications(&caller, q.unread, q.limit, q.offset)
        .await?;
    let unread = state.notifications.unread_count(&caller).await?;
    let total = rows.len() as i64;
    Ok(Json(ListNotificationsDto {
        notifications: rows.into_iter().map(notification_dto).collect(),
        total,
        unread_count: unread,
    }))
}

async fn unread_count(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = id(&req)?;
    let unread = state.notifications.unread_count(&caller).await?;
    Ok(Json(serde_json::json!({ "unread_count": unread })))
}

#[derive(Deserialize)]
pub struct MarkReadBody {
    pub id: Uuid,
}

async fn mark_read(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let body: MarkReadBody = crate::rest::parse_json(req).await?;
    state.notifications.mark_read(&caller, body.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mark_all_read(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = id(&req)?;
    let marked = state.notifications.mark_all_read(&caller).await?;
    Ok(Json(serde_json::json!({ "marked": marked })))
}

// ---------------------------------------------------------------------------
// Device push tokens (FCM)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterDeviceBody {
    pub token: String,
    #[serde(default)]
    pub platform: String,
}

async fn register_device(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let body: RegisterDeviceBody = crate::rest::parse_json(req).await?;
    state
        .notifications
        .register_device(&caller, &body.token, &body.platform)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UnregisterDeviceBody {
    pub token: String,
}

async fn unregister_device(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let body: UnregisterDeviceBody = crate::rest::parse_json(req).await?;
    state
        .notifications
        .unregister_device(&caller, &body.token)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
