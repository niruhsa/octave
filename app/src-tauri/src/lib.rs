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
pub mod downloads;
pub mod error;
pub mod library;
pub mod player;
pub mod playlists;
pub mod assets;
pub mod sync;
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
        .plugin(tauri_plugin_dialog::init())
        // Phase 8 — Uploads: `fs` reads picked files (incl. Android
        // `content://` URIs) for staging; `notification` drives the
        // background-upload progress + completion notifications.
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        // Phase 4 — Playback: `media://<track_id>` serves a cached local
        // file (range-aware) or proxies the server stream with auth. The
        // webview's `<audio>` element loads these URLs directly.
        .register_asynchronous_uri_scheme_protocol(player::stream::SCHEME, player::stream::handle)
        // Phase 6 — Downloads: `cover://<album_id>` serves a downloaded
        // album cover from app-private storage to the webview's `<img>`.
        .register_asynchronous_uri_scheme_protocol(assets::SCHEME, assets::handle)
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
            commands::auth_commands::auth_register,
            commands::auth_commands::auth_change_password,
            commands::auth_commands::auth_list_users,
            commands::auth_commands::auth_delete_user,
            commands::auth_commands::auth_delete_user,
            // library browse + search
            commands::library_commands::library_list_artists,
            commands::library_commands::library_search_artists,
            commands::library_commands::library_list_albums_by_artist,
            commands::library_commands::library_search_albums,
            commands::library_commands::library_list_tracks_by_album,
            commands::library_commands::library_search_tracks,
            // library delete (Phase 8+ — Manager+ gated server-side)
            commands::library_commands::library_delete_artist,
            commands::library_commands::library_delete_album,
            commands::library_commands::library_delete_track,
            commands::library_commands::library_rescan,
            // playlists (Phase 7)
            commands::playlist_commands::playlist_list,
            commands::playlist_commands::playlist_get,
            commands::playlist_commands::playlist_create,
            commands::playlist_commands::playlist_rename,
            commands::playlist_commands::playlist_delete,
            commands::playlist_commands::playlist_add_track,
            commands::playlist_commands::playlist_remove_track,
            commands::playlist_commands::playlist_reorder_track,
            // playback (Phase 4)
            commands::player_commands::player_media_url,
            // sync engine (Phase 5)
            commands::sync_commands::sync_now,
            commands::sync_commands::sync_pending_count,
            commands::sync_commands::sync_enqueue_op,
            // downloads (Phase 6)
            commands::download_commands::download_track,
            commands::download_commands::download_album,
            commands::download_commands::download_playlist,
            commands::download_commands::download_delete,
            commands::download_commands::downloads_storage_usage,
            commands::download_commands::downloads_dir,
            commands::download_commands::downloads_set_dir,
            commands::download_commands::downloads_wifi_only,
            commands::download_commands::downloads_set_wifi_only,
            // uploads (Phase 8) — background jobs + notifications
            commands::upload_commands::upload_files,
            commands::upload_commands::upload_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
