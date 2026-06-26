//! `#[tauri::command]` bridge — the only surface React calls into Rust through.
//!
//! Pattern: frontend `invoke("name", args)` → handler here → typed `AppResult<T>`.
//! Keep handlers thin; delegate heavy work to `db/`, `transport/`, `cache/`.

pub mod auth_commands;
pub mod cache_commands;
pub mod download_commands;
pub mod library_commands;
pub mod notification_commands;
pub mod playlist_commands;
pub mod player_commands;
pub mod podcast_commands;
pub mod sync_commands;
pub mod upload_commands;

use serde::Serialize;

use crate::error::AppResult;

/// Build/runtime info — minimal payload to verify the bridge end-to-end.
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub tauri_version: &'static str,
}

/// Return basic app identity. Used by the frontend smoke test in Phase 0.
#[tauri::command]
pub fn app_info() -> AppResult<AppInfo> {
    Ok(AppInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        tauri_version: tauri::VERSION,
    })
}
