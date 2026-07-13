//! Lyrics commands (Phase 15) — parity with the server lyrics endpoints, with
//! an offline SQLite fallback for downloaded (or previously-viewed) tracks.

use std::sync::Arc;

use tauri::State;

use crate::auth::AuthManager;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::transport::Lyrics;
use crate::AppStateHandle;

async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

/// A track's parsed lyrics. Server-first (freshest); on a transport failure
/// (offline) falls back to the local SQLite mirror. A successful online read
/// refreshes that mirror so a later offline open still has them.
#[tauri::command]
pub async fn get_lyrics(state: State<'_, AppStateHandle>, track_id: String) -> AppResult<Lyrics> {
    let mgr = manager(&state).await?;
    match mgr.get_lyrics(&track_id).await {
        Ok(lyrics) => {
            // Refresh the offline mirror (best-effort — never fail the read).
            let _ = repo::upsert_track_lyrics(&state.pool, &track_id, &lyrics).await;
            Ok(lyrics)
        }
        Err(e) if matches!(e, AppError::Transport(_)) => {
            match repo::get_track_lyrics(&state.pool, &track_id).await? {
                Some(cached) => Ok(cached),
                None => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Manager: force a re-resolve of a track's lyrics.
#[tauri::command]
pub async fn refetch_lyrics(
    state: State<'_, AppStateHandle>,
    track_id: String,
) -> AppResult<Lyrics> {
    let lyrics = manager(&state).await?.refetch_lyrics(&track_id).await?;
    let _ = repo::upsert_track_lyrics(&state.pool, &track_id, &lyrics).await;
    Ok(lyrics)
}

/// Manager: set lyrics from an uploaded `.lrc`/text blob.
#[tauri::command]
pub async fn set_lyrics(
    state: State<'_, AppStateHandle>,
    track_id: String,
    lrc: String,
) -> AppResult<Lyrics> {
    let lyrics = manager(&state).await?.set_lyrics(&track_id, &lrc).await?;
    let _ = repo::upsert_track_lyrics(&state.pool, &track_id, &lyrics).await;
    Ok(lyrics)
}

/// Manager: clear a track's lyrics.
#[tauri::command]
pub async fn clear_lyrics(state: State<'_, AppStateHandle>, track_id: String) -> AppResult<Lyrics> {
    let lyrics = manager(&state).await?.clear_lyrics(&track_id).await?;
    let _ = repo::upsert_track_lyrics(&state.pool, &track_id, &lyrics).await;
    Ok(lyrics)
}
