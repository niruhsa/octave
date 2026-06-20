//! Tauri commands exposing the offline cache to the frontend.
//!
//! React calls these via `invoke("cache_*", args)`. They are thin wrappers
//! around `crate::cache::repo` so the heavy lifting stays Rust-side and
//! testable without Tauri.

use tauri::State;

use crate::cache::model::{Album, AlbumArt, Artist, Playlist, PlaylistTrack, SyncState, Track};
use crate::cache::repo;
use crate::error::AppResult;
use crate::AppStateHandle;

// ---------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_artist(state: State<'_, AppStateHandle>, artist: Artist) -> AppResult<()> {
    repo::upsert_artist(&state.pool, &artist).await
}

#[tauri::command]
pub async fn cache_get_artist(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<Option<Artist>> {
    repo::get_artist(&state.pool, &id).await
}

#[tauri::command]
pub async fn cache_list_artists(state: State<'_, AppStateHandle>) -> AppResult<Vec<Artist>> {
    repo::list_artists(&state.pool).await
}

#[tauri::command]
pub async fn cache_delete_artist(state: State<'_, AppStateHandle>, id: String) -> AppResult<()> {
    repo::delete_artist(&state.pool, &id).await
}

// ---------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_album(state: State<'_, AppStateHandle>, album: Album) -> AppResult<()> {
    repo::upsert_album(&state.pool, &album).await
}

#[tauri::command]
pub async fn cache_get_album(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<Option<Album>> {
    repo::get_album(&state.pool, &id).await
}

#[tauri::command]
pub async fn cache_list_albums_by_artist(
    state: State<'_, AppStateHandle>,
    artist_id: String,
) -> AppResult<Vec<Album>> {
    repo::list_albums_by_artist(&state.pool, &artist_id).await
}

#[tauri::command]
pub async fn cache_delete_album(state: State<'_, AppStateHandle>, id: String) -> AppResult<()> {
    repo::delete_album(&state.pool, &id).await
}

// ---------------------------------------------------------------------------
// album_art
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_album_art(
    state: State<'_, AppStateHandle>,
    art: AlbumArt,
) -> AppResult<()> {
    repo::upsert_album_art(&state.pool, &art).await
}

#[tauri::command]
pub async fn cache_get_album_art(
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<Option<AlbumArt>> {
    repo::get_album_art(&state.pool, &album_id).await
}

#[tauri::command]
pub async fn cache_delete_album_art(
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<()> {
    repo::delete_album_art(&state.pool, &album_id).await
}

// ---------------------------------------------------------------------------
// tracks
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_track(state: State<'_, AppStateHandle>, track: Track) -> AppResult<()> {
    repo::upsert_track(&state.pool, &track).await
}

#[tauri::command]
pub async fn cache_get_track(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<Option<Track>> {
    repo::get_track(&state.pool, &id).await
}

#[tauri::command]
pub async fn cache_list_tracks_by_album(
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<Vec<Track>> {
    repo::list_tracks_by_album(&state.pool, &album_id).await
}

#[tauri::command]
pub async fn cache_list_downloaded_tracks(
    state: State<'_, AppStateHandle>,
) -> AppResult<Vec<Track>> {
    repo::list_downloaded_tracks(&state.pool).await
}

#[tauri::command]
pub async fn cache_delete_track(state: State<'_, AppStateHandle>, id: String) -> AppResult<()> {
    repo::delete_track(&state.pool, &id).await
}

// ---------------------------------------------------------------------------
// playlists
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_playlist(
    state: State<'_, AppStateHandle>,
    playlist: Playlist,
) -> AppResult<()> {
    repo::upsert_playlist(&state.pool, &playlist).await
}

#[tauri::command]
pub async fn cache_list_playlists(state: State<'_, AppStateHandle>) -> AppResult<Vec<Playlist>> {
    repo::list_playlists(&state.pool).await
}

#[tauri::command]
pub async fn cache_delete_playlist(state: State<'_, AppStateHandle>, id: String) -> AppResult<()> {
    repo::delete_playlist(&state.pool, &id).await
}

#[tauri::command]
pub async fn cache_replace_playlist_tracks(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
    entries: Vec<PlaylistTrack>,
) -> AppResult<()> {
    repo::replace_playlist_tracks(&state.pool, &playlist_id, &entries).await
}

#[tauri::command]
pub async fn cache_list_playlist_tracks(
    state: State<'_, AppStateHandle>,
    playlist_id: String,
) -> AppResult<Vec<PlaylistTrack>> {
    repo::list_playlist_tracks(&state.pool, &playlist_id).await
}

// ---------------------------------------------------------------------------
// sync_state
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn cache_upsert_sync_state(
    state: State<'_, AppStateHandle>,
    sync: SyncState,
) -> AppResult<()> {
    repo::upsert_sync_state(&state.pool, &sync).await
}

#[tauri::command]
pub async fn cache_get_sync_state(
    state: State<'_, AppStateHandle>,
    entity_type: String,
    entity_id: String,
) -> AppResult<Option<SyncState>> {
    repo::get_sync_state(&state.pool, &entity_type, &entity_id).await
}
