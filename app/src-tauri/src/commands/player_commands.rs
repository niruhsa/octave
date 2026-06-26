//! Tauri commands for playback (Phase 4).
//!
//! Thin surface over [`crate::player`]. The heavy lifting (the loopback media
//! server, source resolution) lives in `player::server` — this command just
//! hands the frontend the URL string its `<audio>` element loads. Defined here
//! (not re-exported) so the `generate_handler!` macro can find the
//! `__cmd__player_media_url` shim it generates.

use crate::error::AppResult;
use crate::player::server::{MediaServer, action_base_url, cover_url, episode_media_url, media_url};

/// Return the webview-loadable URL for a track (or podcast episode) id. It
/// targets the in-app loopback HTTP server (see `player::server`), which streams
/// a local file or proxies the server stream — the frontend never branches on
/// online/offline. Pass `kind = "episode"` for a podcast episode.
#[tauri::command]
pub async fn player_media_url(
    media: tauri::State<'_, MediaServer>,
    track_id: String,
    kind: Option<String>,
) -> AppResult<String> {
    if kind.as_deref() == Some("episode") {
        Ok(episode_media_url(media.port, &media.token, &track_id))
    } else {
        Ok(media_url(media.port, &media.token, &track_id))
    }
}

/// Return the loopback URL for an album's cover art. Used by the native
/// media-session notification (Android), which fetches the bitmap over real
/// HTTP — it can't reach the webview's `cover://` scheme.
#[tauri::command]
pub async fn player_cover_url(
    media: tauri::State<'_, MediaServer>,
    album_id: String,
) -> AppResult<String> {
    Ok(cover_url(media.port, &media.token, &album_id))
}

/// Base loopback URL the native media notification posts transport actions to.
/// The frontend hands this to the native side (via `media_session_update`).
#[tauri::command]
pub async fn player_action_url_base(
    media: tauri::State<'_, MediaServer>,
) -> AppResult<String> {
    Ok(action_base_url(media.port, &media.token))
}

/// Prefetch the next track to a local temp file so it can advance with the
/// screen off. The webview calls this when a track starts playing; the work
/// happens in the background and is idempotent (see `player::prefetch`).
#[tauri::command]
pub async fn player_prefetch(app: tauri::AppHandle, track_id: String) -> AppResult<()> {
    crate::player::prefetch::spawn(app, track_id);
    Ok(())
}
