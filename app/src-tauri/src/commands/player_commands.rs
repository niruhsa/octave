//! Tauri commands for playback (Phase 4).
//!
//! Thin surface over [`crate::player`]. The heavy lifting (the loopback media
//! server, source resolution) lives in `player::server` — this command just
//! hands the frontend the URL string its `<audio>` element loads. Defined here
//! (not re-exported) so the `generate_handler!` macro can find the
//! `__cmd__player_media_url` shim it generates.

use crate::error::AppResult;
use crate::player::server::{MediaServer, media_url};

/// Return the webview-loadable URL for a track id. It targets the in-app
/// loopback HTTP server (see `player::server`), which streams a local file or
/// proxies the server stream — the frontend never branches on online/offline.
#[tauri::command]
pub async fn player_media_url(
    media: tauri::State<'_, MediaServer>,
    track_id: String,
) -> AppResult<String> {
    Ok(media_url(media.port, &media.token, &track_id))
}
