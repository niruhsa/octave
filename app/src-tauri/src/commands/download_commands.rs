//! Tauri commands for offline downloads (Phase 6).
//!
//! Each command constructs a fresh `DownloadManager` from app state (the
//! manager is cheap to build — one reqwest client + a path lookup — and
//! per-call construction means a settings change like the downloads-root
//! override takes effect immediately on the next command).

use tauri::{AppHandle, State};

use crate::downloads::{
    BatchDownloadResult, DownloadManager, StorageUsage, TrackDownloadResult,
};
use crate::error::{AppError, AppResult};
use crate::AppStateHandle;

async fn manager(
    app: AppHandle,
    state: &State<'_, AppStateHandle>,
) -> AppResult<DownloadManager> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    DownloadManager::new(app, state.pool.clone(), auth).await
}

/// Download one track. Emits `download-progress` events as it goes.
#[tauri::command]
pub async fn download_track(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    track_id: String,
) -> AppResult<TrackDownloadResult> {
    // Android: bring up the download foreground service while we're still
    // foreground (the user just tapped Download) so the transfer survives the
    // app being backgrounded / the screen locking — Android otherwise severs
    // background network almost immediately. The guard stops it (releasing the
    // wake / WiFi locks + notification) on every exit path. No-op on desktop.
    crate::download_session::start(&app, "Downloading music", "1 track", -1);
    let _fg = crate::download_session::ForegroundGuard::new(app.clone());
    manager(app, &state).await?.download_track(&track_id).await
}

/// Download every track in an album (skips already-downloaded ones).
#[tauri::command]
pub async fn download_album(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    album_id: String,
) -> AppResult<BatchDownloadResult> {
    // Foreground service for the duration of the batch (see `download_track`).
    crate::download_session::start(&app, "Downloading album", "Preparing…", -1);
    let _fg = crate::download_session::ForegroundGuard::new(app.clone());
    manager(app, &state).await?.download_album(&album_id).await
}

/// Download every track in a playlist (deduped; skips downloaded ones).
#[tauri::command]
pub async fn download_playlist(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    playlist_id: String,
) -> AppResult<BatchDownloadResult> {
    // Foreground service for the duration of the batch (see `download_track`).
    crate::download_session::start(&app, "Downloading playlist", "Preparing…", -1);
    let _fg = crate::download_session::ForegroundGuard::new(app.clone());
    manager(app, &state).await?.download_playlist(&playlist_id).await
}

/// Remove a downloaded track (file + cache row; cover pruned if the album
/// is now empty).
#[tauri::command]
pub async fn download_delete(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    track_id: String,
) -> AppResult<()> {
    manager(app, &state).await?.delete_track(&track_id).await
}

// ----- podcasts ----------------------------------------------------------

/// Download one podcast episode for offline use (same resumable path as a
/// track). Triggers the server-side fetch, then streams the server copy.
#[tauri::command]
pub async fn podcast_download_episode(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    episode_id: String,
) -> AppResult<TrackDownloadResult> {
    crate::download_session::start(&app, "Downloading podcast", "1 episode", -1);
    let _fg = crate::download_session::ForegroundGuard::new(app.clone());
    manager(app, &state).await?.download_episode(&episode_id).await
}

/// Download the newest N not-yet-downloaded episodes of a show (default 10).
#[tauri::command]
pub async fn podcast_download_show(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    podcast_id: String,
    newest_n: Option<u32>,
) -> AppResult<BatchDownloadResult> {
    crate::download_session::start(&app, "Downloading podcast", "Preparing…", -1);
    let _fg = crate::download_session::ForegroundGuard::new(app.clone());
    manager(app, &state)
        .await?
        .download_podcast(&podcast_id, newest_n)
        .await
}

/// Remove a downloaded episode (file + cache row).
#[tauri::command]
pub async fn podcast_delete_episode(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    episode_id: String,
) -> AppResult<()> {
    manager(app, &state).await?.delete_episode(&episode_id).await
}

/// Total bytes + row counts used by offline content.
#[tauri::command]
pub async fn downloads_storage_usage(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
) -> AppResult<StorageUsage> {
    manager(app, &state).await?.storage_usage().await
}

/// Current downloads root (resolved absolute path).
#[tauri::command]
pub async fn downloads_dir(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
) -> AppResult<String> {
    Ok(manager(app, &state).await?.root().to_string_lossy().into_owned())
}

/// Override the downloads root (desktop: user-chosen location). Persisted
/// in settings; takes effect on the next command.
#[tauri::command]
pub async fn downloads_set_dir(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    path: String,
) -> AppResult<()> {
    manager(app, &state).await?.set_root(&path).await
}

/// Mobile Wi-Fi-only toggle state.
#[tauri::command]
pub async fn downloads_wifi_only(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
) -> AppResult<bool> {
    manager(app, &state).await?.wifi_only().await
}

#[tauri::command]
pub async fn downloads_set_wifi_only(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    on: bool,
) -> AppResult<()> {
    manager(app, &state).await?.set_wifi_only(on).await
}
