//! Tauri commands for library browse + search.
//!
//! All calls go through `LibraryService`, which decides per-call whether
//! to hit the server or fall back to the cache. The frontend never has to
//! ask "am I online?" — the returned `LibraryView` carries a `source` tag.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::library::{LibraryView, MergedAlbum, MergedArtist, MergedTrack};
use crate::library::service::LibraryService;
use crate::AppStateHandle;

/// Server default page sizes mirror the server's cap (200) / default (50).
const DEFAULT_LIMIT: i64 = 50;

async fn service<'a>(state: &'a State<'a, AppStateHandle>) -> AppResult<LibraryService<'a>> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    Ok(LibraryService::new(&state.pool, auth))
}

fn normalise_limit(limit: Option<i64>) -> i64 {
    let l = limit.unwrap_or(DEFAULT_LIMIT);
    if l <= 0 { DEFAULT_LIMIT } else { l.min(200) }
}

fn normalise_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

// ---------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_artists(
    state: State<'_, AppStateHandle>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedArtist>> {
    let svc = service(&state).await?;
    svc.list_artists(normalise_limit(limit), normalise_offset(offset)).await
}

#[tauri::command]
pub async fn library_search_artists(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedArtist>> {
    let svc = service(&state).await?;
    svc.search_artists(&query, normalise_limit(limit), normalise_offset(offset)).await
}

// ---------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_albums_by_artist(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<LibraryView<MergedAlbum>> {
    let svc = service(&state).await?;
    svc.list_albums_by_artist(&artist_id).await
}

#[tauri::command]
pub async fn library_search_albums(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedAlbum>> {
    let svc = service(&state).await?;
    svc.search_albums(&query, normalise_limit(limit), normalise_offset(offset)).await
}

// ---------------------------------------------------------------------------
// tracks
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn library_list_tracks_by_album(
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<LibraryView<MergedTrack>> {
    let svc = service(&state).await?;
    svc.list_tracks_by_album(&album_id).await
}

#[tauri::command]
pub async fn library_search_tracks(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<LibraryView<MergedTrack>> {
    let svc = service(&state).await?;
    svc.search_tracks(&query, normalise_limit(limit), normalise_offset(offset)).await
}
