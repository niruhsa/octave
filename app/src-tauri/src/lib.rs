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
pub mod media_session;
pub mod player;
pub mod playlists;
pub mod assets;
pub mod sync;
pub mod transport;
pub mod upload_session;

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
        // Native Android media notification (shade + lock screen) + transport.
        // Binds the Kotlin `MediaSessionPlugin`; no-op on desktop. See
        // `media_session` for why a bare WebView needs this native bridge.
        .plugin(media_session::init())
        // Native Android upload foreground service. Binds the Kotlin
        // `UploadServicePlugin`; no-op on desktop. Keeps the upload process +
        // network alive (persistent notification + wake/WiFi locks) while the
        // app is backgrounded / screen-locked — see `upload_session`.
        .plugin(upload_session::init())
        // Phase 6 — Downloads: `cover://<album_id>` serves a downloaded
        // album cover from app-private storage to the webview's `<img>`.
        // (Playback no longer uses a custom protocol — see the loopback HTTP
        // server started in `setup` and `player::server`.)
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

            // Pause/cancel control for the active upload job (one at a time).
            app.manage(commands::upload_commands::UploadControl::default());

            // Phase 4 — Playback: start the in-app loopback HTTP server the
            // webview's `<audio>` element streams media from (see
            // `player::server`). Bind synchronously so the port is known before
            // any `player_media_url` call, then serve on the async runtime.
            let token: Arc<str> = uuid::Uuid::new_v4().to_string().into();
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("bind media server: {e}"))?;
            listener
                .set_nonblocking(true)
                .map_err(|e| format!("media server nonblocking: {e}"))?;
            let port = listener
                .local_addr()
                .map_err(|e| format!("media server addr: {e}"))?
                .port();

            let app_handle = app.handle().clone();
            let server_token = token.clone();
            tauri::async_runtime::spawn(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!(error = %e, "media server: adopt listener failed");
                        return;
                    }
                };
                let router = player::server::router(app_handle, server_token);
                if let Err(e) = axum::serve(listener, router.into_make_service()).await {
                    tracing::error!(error = %e, "media server stopped");
                }
            });

            app.manage(player::server::MediaServer {
                port,
                token: token.to_string(),
            });
            tracing::info!(port, "media server listening on 127.0.0.1");

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
            commands::auth_commands::auth_server_config,
            commands::auth_commands::auth_change_server,
            commands::auth_commands::auth_login,
            commands::auth_commands::auth_set_secret_key,
            commands::auth_commands::auth_whoami,
            commands::auth_commands::auth_session,
            commands::auth_commands::auth_logout,
            commands::auth_commands::auth_refresh_online,
            commands::auth_commands::auth_refresh_transports,
            commands::auth_commands::auth_register,
            commands::auth_commands::auth_change_password,
            commands::auth_commands::auth_list_users,
            commands::auth_commands::auth_delete_user,
            // library browse + search
            commands::library_commands::library_list_artists,
            commands::library_commands::library_search_artists,
            commands::library_commands::library_list_albums_by_artist,
            commands::library_commands::library_search_albums,
            commands::library_commands::library_list_tracks_by_album,
            commands::library_commands::library_search_tracks,
            // metadata edit (Phase 9 — Manager+ gated server-side)
            commands::library_commands::library_edit_track_metadata,
            commands::library_commands::library_upload_album_cover,
            commands::library_commands::library_upload_artist_image,
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
            commands::player_commands::player_cover_url,
            commands::player_commands::player_action_url_base,
            // native media session (Android notification + lock screen)
            media_session::media_session_update,
            media_session::media_session_set_playback,
            media_session::media_session_clear,
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
            // uploads (v2) — background jobs + notifications + reports
            commands::upload_commands::upload_files,
            commands::upload_commands::upload_folder,
            commands::upload_commands::uploads_list,
            commands::upload_commands::uploads_get,
            commands::upload_commands::uploads_cancel,
            commands::upload_commands::uploads_pause,
            commands::upload_commands::uploads_resume,
            commands::upload_commands::uploads_subscribe,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
