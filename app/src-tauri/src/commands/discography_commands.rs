//! Tauri commands for discography sync (Phase 14). Thin pass-throughs to
//! `AuthManager`; the server enforces Manager gating (the frontend also hides
//! the panel from non-managers). See DISCOGRAPHY_SYNC.md.

use std::sync::Arc;

use tauri::State;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::transport::{
    DiscographyCandidate, DiscographyIgnore, DiscographyReport, DiscographyStatus,
    DiscographySyncAll, DiscographySyncResult,
};
use crate::AppStateHandle;

async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

/// The cached gap report (`None` when never synced).
#[tauri::command]
pub async fn discography_report(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<Option<DiscographyReport>> {
    manager(&state).await?.discography_report(&artist_id).await
}

/// Trigger a sync — returns a report or a candidate list to disambiguate.
#[tauri::command]
pub async fn discography_sync(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<DiscographySyncResult> {
    manager(&state).await?.discography_sync(&artist_id).await
}

/// Provider artist candidates for the disambiguation dialog.
#[tauri::command]
pub async fn discography_candidates(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<Vec<DiscographyCandidate>> {
    manager(&state).await?.discography_candidates(&artist_id).await
}

/// Pin the artist ↔ provider match (or ignore the artist when `mbid` is null).
#[tauri::command]
pub async fn discography_resolve(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    mbid: Option<String>,
) -> AppResult<()> {
    manager(&state)
        .await?
        .discography_resolve(&artist_id, mbid.as_deref())
        .await
}

/// The artist's suppression list.
#[tauri::command]
pub async fn discography_ignores(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<Vec<DiscographyIgnore>> {
    manager(&state).await?.discography_ignores(&artist_id).await
}

/// Suppress a release/track; returns the re-filtered report.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn discography_add_ignore(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    scope: String,
    release_group_id: String,
    recording_id: Option<String>,
    title_key: Option<String>,
    label: String,
) -> AppResult<DiscographyReport> {
    manager(&state)
        .await?
        .discography_add_ignore(
            &artist_id,
            &scope,
            &release_group_id,
            recording_id.as_deref(),
            title_key.as_deref(),
            &label,
        )
        .await
}

/// Remove a suppression; returns the re-filtered report.
#[tauri::command]
pub async fn discography_remove_ignore(
    state: State<'_, AppStateHandle>,
    artist_id: String,
    ignore_id: String,
) -> AppResult<DiscographyReport> {
    manager(&state)
        .await?
        .discography_remove_ignore(&artist_id, &ignore_id)
        .await
}

/// Library-wide coverage for the admin dashboard.
#[tauri::command]
pub async fn discography_status(
    state: State<'_, AppStateHandle>,
) -> AppResult<DiscographyStatus> {
    manager(&state).await?.discography_status().await
}

/// Re-sync every matched artist (rate-limited; can take a while).
#[tauri::command]
pub async fn discography_sync_all(
    state: State<'_, AppStateHandle>,
) -> AppResult<DiscographySyncAll> {
    manager(&state).await?.discography_sync_all().await
}
