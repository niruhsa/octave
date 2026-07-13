//! Tauri commands for follows & notifications (Phase 10).
//!
//! Thin pass-throughs to [`AuthManager`] (like the auth/user commands) — these
//! are server-authoritative and online-only: following an artist and the
//! notification feed live on the server, so there's no offline cache path.
//! The frontend gates the affordances (only a logged-in *user* — not a
//! `SECRET_KEY` session — can follow), but the server re-enforces it.

use std::sync::Arc;

use tauri::State;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::transport::{Artist, NotificationPage};
use crate::AppStateHandle;

/// Resolve the active `AuthManager`, or error if no server is configured yet.
async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

// ----- Follows -------------------------------------------------------------

/// Follow an artist. Returns the resulting follow state (`true`).
#[tauri::command]
pub async fn follow_artist(state: State<'_, AppStateHandle>, artist_id: String) -> AppResult<bool> {
    manager(&state).await?.follow_artist(&artist_id).await
}

/// Unfollow an artist. Returns the resulting follow state (`false`).
#[tauri::command]
pub async fn unfollow_artist(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<bool> {
    manager(&state).await?.unfollow_artist(&artist_id).await
}

/// Whether the caller currently follows `artist_id`.
#[tauri::command]
pub async fn is_following(state: State<'_, AppStateHandle>, artist_id: String) -> AppResult<bool> {
    manager(&state).await?.is_following(&artist_id).await
}

/// The artists the caller follows (slim projection — id/name/image).
#[tauri::command]
pub async fn list_following(state: State<'_, AppStateHandle>) -> AppResult<Vec<Artist>> {
    manager(&state).await?.list_following().await
}

// ----- Notifications -------------------------------------------------------

/// A page of the caller's notifications (newest first) + the total unread
/// count for a badge. `limit`/`offset` default server-side when omitted.
#[tauri::command]
pub async fn notifications_list(
    state: State<'_, AppStateHandle>,
    unread_only: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<NotificationPage> {
    manager(&state)
        .await?
        .list_notifications(unread_only.unwrap_or(false), limit, offset)
        .await
}

/// The caller's unread notification count (for the sidebar/badge).
#[tauri::command]
pub async fn notifications_unread_count(state: State<'_, AppStateHandle>) -> AppResult<i64> {
    manager(&state).await?.notifications_unread_count().await
}

/// Mark one notification read. 404s server-side if it isn't the caller's.
#[tauri::command]
pub async fn notifications_mark_read(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<()> {
    manager(&state).await?.mark_notification_read(&id).await
}

/// Mark every unread notification read. Returns the count flipped.
#[tauri::command]
pub async fn notifications_mark_all_read(state: State<'_, AppStateHandle>) -> AppResult<u64> {
    manager(&state).await?.mark_all_notifications_read().await
}
