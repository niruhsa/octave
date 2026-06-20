//! Tauri commands for playback (Phase 4).
//!
//! Thin surface over [`crate::player`]. The actual heavy lifting (media
//! protocol, source resolution) lives in `player::stream` — this command
//! just hands the frontend the platform-correct URL string. Defined here
//! (not re-exported) so the `generate_handler!` macro can find the
//! `__cmd__player_media_url` shim it generates.

use crate::error::AppResult;
use crate::player::resolver::media_url;

/// Return the webview-loadable URL for a track id. The URL targets the
/// `media://` protocol registered in `lib.rs`, which resolves to a local
/// file or a proxied server stream — the frontend never branches.
#[tauri::command]
pub async fn player_media_url(track_id: String) -> AppResult<String> {
    Ok(media_url(&track_id))
}
