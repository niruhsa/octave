//! Tauri commands for favorites (Phase 11).
//!
//! Thin pass-throughs to [`AuthManager`] (like follows/notifications) — favorites
//! are server-authoritative and online-only (the frontend toggles optimistically
//! and reverts on error). Only a logged-in *user* can favorite; the server
//! re-enforces it. `kind` is `"track"` | `"album"` | `"artist"`.

use std::sync::Arc;

use tauri::State;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::transport::{Album, Artist, Track};
use crate::AppStateHandle;

async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

#[tauri::command]
pub async fn favorites_favorite(
    state: State<'_, AppStateHandle>,
    kind: String,
    entity_id: String,
) -> AppResult<bool> {
    manager(&state).await?.favorite(&kind, &entity_id).await
}

#[tauri::command]
pub async fn favorites_unfavorite(
    state: State<'_, AppStateHandle>,
    kind: String,
    entity_id: String,
) -> AppResult<bool> {
    manager(&state).await?.unfavorite(&kind, &entity_id).await
}

#[tauri::command]
pub async fn favorites_is_favorite(
    state: State<'_, AppStateHandle>,
    kind: String,
    entity_id: String,
) -> AppResult<bool> {
    manager(&state).await?.is_favorite(&kind, &entity_id).await
}

#[tauri::command]
pub async fn favorites_list_tracks(state: State<'_, AppStateHandle>) -> AppResult<Vec<Track>> {
    manager(&state).await?.list_favorite_tracks().await
}

#[tauri::command]
pub async fn favorites_list_albums(state: State<'_, AppStateHandle>) -> AppResult<Vec<Album>> {
    manager(&state).await?.list_favorite_albums().await
}

#[tauri::command]
pub async fn favorites_list_artists(state: State<'_, AppStateHandle>) -> AppResult<Vec<Artist>> {
    manager(&state).await?.list_favorite_artists().await
}

/// Just the favorited track ids — for bulk heart-state hydration in the UI.
#[tauri::command]
pub async fn favorites_track_ids(state: State<'_, AppStateHandle>) -> AppResult<Vec<String>> {
    manager(&state).await?.favorited_track_ids().await
}
