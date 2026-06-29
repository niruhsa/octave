//! Tauri commands for recommendations / discover (Phase 11).
//!
//! Thin pass-throughs to [`AuthManager`]. Server-authoritative + online-only:
//! `discover_home` is personalized (bearer-user); `discover_radio` returns a
//! seeded track queue the client hands to the player.

use std::sync::Arc;

use tauri::State;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::transport::{DiscoverSection, Track};
use crate::AppStateHandle;

async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

/// Personalized home shelves (only the non-empty ones).
#[tauri::command]
pub async fn discover_home(state: State<'_, AppStateHandle>) -> AppResult<Vec<DiscoverSection>> {
    manager(&state).await?.discover_home().await
}

/// A radio queue seeded from an artist or an album (pass exactly one id).
#[tauri::command]
pub async fn discover_radio(
    state: State<'_, AppStateHandle>,
    seed_artist_id: Option<String>,
    seed_album_id: Option<String>,
) -> AppResult<Vec<Track>> {
    manager(&state)
        .await?
        .discover_radio(seed_artist_id.as_deref(), seed_album_id.as_deref())
        .await
}
