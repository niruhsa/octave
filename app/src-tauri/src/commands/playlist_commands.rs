//! Tauri commands for playlist management (Phase 7).
//!
//! All calls go through `PlaylistService`, which decides per-call whether
//! to push the mutation straight to the server or queue it as a
//! `PendingOpKind` for replay. The frontend never has to ask "am I online?"
//! — reads return a `LibrarySource` tag and mutations either land on the
//! server immediately or are mirrored optimistically into the cache.

use std::sync::Arc;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::library::LibraryView;
use crate::playlists::{PlaylistDetailView, PlaylistService, MergedPlaylist};
use crate::AppStateHandle;

async fn service<'a>(state: &'a State<'a, AppStateHandle>) -> AppResult<PlaylistService<'a>> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    Ok(PlaylistService::new(&state.pool, Arc::clone(&auth)))
}

/// Current user's playlists. Online → server, mirrored to cache; offline → cache.
#[tauri::command]
pub async fn playlist_list(
    state: State<'_, AppStateHandle>,
) -> AppResult<LibraryView<MergedPlaylist>> {
    service(&state).await?.list_my_playlists().await
}

/// One playlist + its ordered entries. `None` when not found on the
/// resolved source.
#[tauri::command]
pub async fn playlist_get(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
) -> AppResult<Option<PlaylistDetailView>> {
    service(&state).await?.get_playlist(&playlist_id).await
}

/// Create a playlist. Online → server-issued id; offline → `local:` id +
/// queued `playlist.create` op. `SECRET_KEY` sessions are rejected for
/// offline creates (no `user_id` to own the row).
#[tauri::command]
pub async fn playlist_create(
    state: State<'_, AppStateHandle>,
    name: String,
) -> AppResult<MergedPlaylist> {
    service(&state).await?.create_playlist(&name).await
}

/// Rename. Server-known id → server rename + cache mirror; `local:` id or
/// offline → queued op + optimistic cache update.
#[tauri::command]
pub async fn playlist_rename(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
    name: String,
) -> AppResult<MergedPlaylist> {
    service(&state)
        .await?
        .rename_playlist(&playlist_id, &name)
        .await
}

/// Delete. Server-known id → server delete + cache prune; `local:` id or
/// offline → queued op + optimistic cache delete (and, for a `local:` id,
/// any dependent queued ops are dropped too).
#[tauri::command]
pub async fn playlist_delete(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
) -> AppResult<()> {
    service(&state).await?.delete_playlist(&playlist_id).await
}

/// Add a track. `position = 0` ⇒ append; `position ≥ 1` ⇒ 1-based insert
/// with shift. Returns the refreshed detail view.
#[tauri::command]
pub async fn playlist_add_track(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
    track_id: String,
    position: i32,
) -> AppResult<PlaylistDetailView> {
    service(&state)
        .await?
        .add_track(&playlist_id, &track_id, position)
        .await
}

/// Remove the entry at 1-based `position`. Returns the refreshed detail.
#[tauri::command]
pub async fn playlist_remove_track(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
    position: i32,
) -> AppResult<PlaylistDetailView> {
    service(&state)
        .await?
        .remove_track(&playlist_id, position)
        .await
}

/// Move the entry at 1-based `from_position` to 1-based `to_position`.
/// Returns the refreshed detail.
#[tauri::command]
pub async fn playlist_reorder_track(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
    from_position: i32,
    to_position: i32,
) -> AppResult<PlaylistDetailView> {
    service(&state)
        .await?
        .reorder_track(&playlist_id, from_position, to_position)
        .await
}
