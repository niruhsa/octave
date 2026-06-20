//! Music app — Rust native core.
//!
//! Modules:
//! - [`commands`] — `#[tauri::command]` bridge exposed to React.
//! - [`db`]       — local SQLite offline cache pool + migrations.
//! - [`transport`]— gRPC-primary / REST-fallback server client.
//! - [`cache`]    — typed cache repository on top of `db::SqlitePool`.
//! - [`auth`]     — credential storage + session manager.
//! - [`error`]    — shared `AppError` / `AppResult` types.

pub mod auth;
pub mod cache;
pub mod commands;
pub mod db;
pub mod error;
pub mod library;
pub mod transport;

use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::Manager;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use crate::auth::AuthManager;

/// Process-wide app state shared with every command.
///
/// * `pool`  — SQLite offline cache (always present).
/// * `auth`  — server transport + auth manager, populated once the user
///   has supplied a server URL via `auth_configure_server`.
pub struct AppStateHandle {
    pub pool: SqlitePool,
    pub auth: RwLock<Option<Arc<AuthManager>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Structured logs; override via `RUST_LOG=music_app_lib=debug`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,music_app_lib=debug")),
        )
        .try_init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting music-app");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app_data_dir: {e}"))?;
            let db_path = db::default_db_path(&app_data_dir);

            let pool = tauri::async_runtime::block_on(db::open(&db_path))
                .map_err(|e| format!("open cache db at {}: {e:?}", db_path.display()))?;

            app.manage(AppStateHandle {
                pool,
                auth: RwLock::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            // cache: artists
            commands::cache_commands::cache_upsert_artist,
            commands::cache_commands::cache_get_artist,
            commands::cache_commands::cache_list_artists,
            commands::cache_commands::cache_delete_artist,
            // cache: albums
            commands::cache_commands::cache_upsert_album,
            commands::cache_commands::cache_get_album,
            commands::cache_commands::cache_list_albums_by_artist,
            commands::cache_commands::cache_delete_album,
            // cache: album_art
            commands::cache_commands::cache_upsert_album_art,
            commands::cache_commands::cache_get_album_art,
            commands::cache_commands::cache_delete_album_art,
            // cache: tracks
            commands::cache_commands::cache_upsert_track,
            commands::cache_commands::cache_get_track,
            commands::cache_commands::cache_list_tracks_by_album,
            commands::cache_commands::cache_list_downloaded_tracks,
            commands::cache_commands::cache_delete_track,
            // cache: playlists
            commands::cache_commands::cache_upsert_playlist,
            commands::cache_commands::cache_list_playlists,
            commands::cache_commands::cache_delete_playlist,
            commands::cache_commands::cache_replace_playlist_tracks,
            commands::cache_commands::cache_list_playlist_tracks,
            // cache: sync_state
            commands::cache_commands::cache_upsert_sync_state,
            commands::cache_commands::cache_get_sync_state,
            // auth + transport
            commands::auth_commands::auth_configure_server,
            commands::auth_commands::auth_login,
            commands::auth_commands::auth_set_secret_key,
            commands::auth_commands::auth_whoami,
            commands::auth_commands::auth_session,
            commands::auth_commands::auth_logout,
            commands::auth_commands::auth_refresh_online,
            // library browse + search
            commands::library_commands::library_list_artists,
            commands::library_commands::library_search_artists,
            commands::library_commands::library_list_albums_by_artist,
            commands::library_commands::library_search_albums,
            commands::library_commands::library_list_tracks_by_album,
            commands::library_commands::library_search_tracks,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
