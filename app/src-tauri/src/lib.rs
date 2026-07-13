//! Music app — Rust native core.
//!
//! Modules:
//! - [`commands`] — `#[tauri::command]` bridge exposed to React.
//! - [`db`]       — local SQLite offline cache pool + migrations.
//! - [`transport`]— gRPC-primary / REST-fallback server client.
//! - [`cache`]    — typed cache repository on top of `db::SqlitePool`.
//! - [`auth`]     — credential storage + session manager.
//! - [`error`]    — shared `AppError` / `AppResult` types.

pub mod assets;
pub mod auth;
pub mod cache;
pub mod commands;
pub mod db;
pub mod download_session;
pub mod downloads;
pub mod equalizer;
pub mod error;
pub mod library;
pub mod media_session;
pub mod notify_sync;
pub mod player;
pub mod playlists;
pub mod podcasts;
pub mod push;
pub mod sync;
pub mod transport;
pub mod upload_session;

/// User-Agent sent on every outbound HTTP request the native core makes
/// (server transport, media/cover proxy, episode + track downloads). Podcast
/// origin CDNs commonly reject requests with no — or a generic library —
/// User-Agent, so every `reqwest` client identifies itself with this.
pub const USER_AGENT: &str = concat!(
    "octave/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/niruhsa/octave)"
);

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::{Emitter, Manager};
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
    pub auth: Arc<RwLock<Option<Arc<AuthManager>>>>,
}

/// Coalesces Android `AudioDeviceCallback` bursts before re-running the native
/// resolver. Desktop adapters perform the same settle check in their monitor.
#[derive(Default)]
pub struct EqualizerRouteDebouncer(AtomicU64);

async fn apply_equalizer_outputs(app: tauri::AppHandle, outputs: Vec<equalizer::AudioOutput>) {
    let service = app
        .state::<Arc<equalizer::EqualizerService>>()
        .inner()
        .clone();
    match service.update_outputs(outputs).await {
        Ok(resolved) => {
            let _ = app.emit("equalizer-effective-changed", resolved);
        }
        Err(error) => tracing::warn!(%error, "resolve changed audio output failed"),
    }
}

/// Re-query the platform adapter and publish only the redacted resolved state.
pub(crate) async fn refresh_equalizer_outputs(app: tauri::AppHandle) {
    match equalizer::audio_output::query_outputs(app.clone()).await {
        Ok(outputs) => apply_equalizer_outputs(app, outputs).await,
        Err(error) => tracing::debug!(%error, "query changed audio output failed"),
    }
}

/// Schedule the token-authenticated Android route hint after the documented
/// 650 ms settle window. Each newer hint invalidates the prior timer.
pub(crate) fn schedule_equalizer_output_refresh(app: tauri::AppHandle) {
    let state = app.state::<EqualizerRouteDebouncer>();
    let generation = state.0.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(650)).await;
        let current = app
            .state::<EqualizerRouteDebouncer>()
            .0
            .load(Ordering::SeqCst);
        if generation == current {
            refresh_equalizer_outputs(app).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Structured logs; override via `RUST_LOG=octave_lib=debug`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,octave_lib=debug")),
        )
        .try_init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting octave");

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
        // Native Android download foreground service. Binds the Kotlin
        // `DownloadServicePlugin`; no-op on desktop. Keeps a download (+ its
        // network) alive while the app is backgrounded / screen-locked — see
        // `download_session`. Mirrors `upload_session`.
        .plugin(download_session::init())
        // Phase 10 — Follows & Notifications: native Android background poll
        // (WorkManager) that surfaces new-release notifications while the app is
        // closed. Binds the Kotlin `NotificationSyncPlugin`; no-op on desktop.
        .plugin(notify_sync::init())
        // Phase 10 — real-time push via FCM. Binds the Kotlin `PushPlugin`
        // (fetch/delete the registration token); no-op on desktop. When FCM is
        // available it supersedes the WorkManager poll above.
        .plugin(push::init())
        // Native output discovery for automatic equalizer profile selection.
        // Android binds the Kotlin AudioManager bridge; desktop adapters use
        // the platform audio APIs directly. Raw endpoint ids never cross IPC.
        .plugin(equalizer::audio_output::init())
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

            let auth = Arc::new(RwLock::new(None));
            let equalizer = Arc::new(equalizer::EqualizerService::new(pool.clone(), auth.clone()));
            app.manage(AppStateHandle { pool, auth });
            app.manage(equalizer);
            app.manage(EqualizerRouteDebouncer::default());

            // Pause/cancel control for the active upload job (one at a time).
            app.manage(commands::upload_commands::UploadControl::default());

            // User-tunable chunk concurrency (Settings → Networking). The client
            // pushes its persisted value on startup via `uploads_set_concurrency`;
            // a change resizes the in-flight upload's concurrency on the fly.
            app.manage(commands::upload_commands::UploadConcurrency::default());

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

            // Android sends only a generic, token-authenticated loopback hint;
            // Rust re-queries the plugin so no endpoint id enters the URL or
            // WebView. Desktop monitors deliver already-queried descriptors.
            if let Err(error) = equalizer::audio_output::configure_callback(app.handle()) {
                tracing::warn!(%error, "configure Android EQ route callback failed");
            }
            let monitor_app = app.handle().clone();
            equalizer::audio_output::spawn_monitor(monitor_app, |app, outputs| async move {
                apply_equalizer_outputs(app, outputs).await
            });
            #[cfg(target_os = "android")]
            tauri::async_runtime::spawn(refresh_equalizer_outputs(app.handle().clone()));

            // Look-ahead prefetch cache: while a streamed track plays we fetch
            // the next one to a temp file so it can be served locally at the
            // track boundary (a hidden WebView won't start a network media
            // load — see `player::prefetch`). Transient; cleared on launch.
            let prefetch_dir = app
                .path()
                .app_cache_dir()
                .unwrap_or_else(|_| app_data_dir.clone())
                .join("octave-prefetch");
            app.manage(player::prefetch::PrefetchCache::new(prefetch_dir));

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
            commands::cache_commands::cache_list_downloaded_episodes,
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
            // synced parametric EQ + device-local output resolution
            commands::equalizer_commands::equalizer_snapshot,
            commands::equalizer_commands::equalizer_sync_now,
            commands::equalizer_commands::equalizer_get_local_preferences,
            commands::equalizer_commands::equalizer_set_local_preferences,
            commands::equalizer_commands::equalizer_set_manual_override,
            commands::equalizer_commands::equalizer_clear_manual_override,
            commands::equalizer_commands::equalizer_create_profile,
            commands::equalizer_commands::equalizer_update_profile,
            commands::equalizer_commands::equalizer_delete_profile,
            commands::equalizer_commands::equalizer_set_default,
            commands::equalizer_commands::equalizer_create_device_rule,
            commands::equalizer_commands::equalizer_update_device_rule,
            commands::equalizer_commands::equalizer_delete_device_rule,
            commands::equalizer_commands::equalizer_reorder_device_rules,
            commands::equalizer_commands::equalizer_promote_local_profile,
            commands::equalizer_commands::equalizer_attach_current_output,
            commands::equalizer_commands::equalizer_detach_current_output,
            commands::equalizer_commands::equalizer_audio_outputs,
            commands::equalizer_commands::equalizer_current_output,
            commands::equalizer_commands::equalizer_conflicts,
            commands::equalizer_commands::equalizer_resolve_conflict,
            commands::equalizer_commands::equalizer_parse_text,
            commands::equalizer_commands::equalizer_import_file,
            commands::equalizer_commands::equalizer_export_file,
            commands::equalizer_commands::equalizer_list_changes,
            commands::equalizer_commands::equalizer_get_change,
            commands::equalizer_commands::equalizer_rollback_change,
            // library browse + search
            commands::library_commands::library_list_artists,
            commands::library_commands::library_search_artists,
            commands::library_commands::library_list_albums_by_artist,
            commands::library_commands::library_search_albums,
            commands::library_commands::library_list_tracks_by_album,
            commands::library_commands::library_search_tracks,
            commands::library_commands::library_get_storage,
            // metadata edit (Phase 9 — Manager+ gated server-side)
            commands::library_commands::library_edit_track_metadata,
            commands::library_commands::library_upload_album_cover,
            commands::library_commands::library_upload_artist_image,
            // single-entity fetch (with alias set) for the Artist/Album routes
            commands::library_commands::library_get_artist,
            commands::library_commands::library_get_album,
            // merge + aliases (Phase 10 — Manager+ gated server-side)
            commands::library_commands::library_merge_artists,
            commands::library_commands::library_list_artist_library_paths,
            commands::library_commands::library_set_artist_language,
            commands::library_commands::library_album_folder,
            commands::library_commands::library_rename_album_folder,
            commands::library_commands::library_merge_albums,
            commands::library_commands::library_move_track,
            commands::library_commands::library_set_track_single_release,
            commands::library_commands::library_set_track_explicit,
            commands::library_commands::library_set_album_type,
            commands::library_commands::library_add_artist_alias,
            commands::library_commands::library_remove_artist_alias,
            commands::library_commands::library_set_primary_artist_alias,
            commands::library_commands::library_add_album_alias,
            commands::library_commands::library_remove_album_alias,
            commands::library_commands::library_set_primary_album_alias,
            commands::library_commands::library_list_track_aliases,
            commands::library_commands::library_add_track_alias,
            commands::library_commands::library_remove_track_alias,
            commands::library_commands::library_set_primary_track_alias,
            // library delete (Phase 8+ — Manager+ gated server-side)
            commands::library_commands::library_delete_artist,
            commands::library_commands::library_delete_album,
            commands::library_commands::library_delete_track,
            commands::library_commands::library_rescan,
            // follows & notifications (Phase 10)
            commands::notification_commands::follow_artist,
            commands::notification_commands::unfollow_artist,
            commands::notification_commands::is_following,
            commands::notification_commands::list_following,
            commands::notification_commands::notifications_list,
            commands::notification_commands::notifications_unread_count,
            commands::notification_commands::notifications_mark_read,
            commands::notification_commands::notifications_mark_all_read,
            // play history (Phase 11)
            commands::play_history_commands::play_history_record,
            commands::play_history_commands::play_history_flush,
            commands::play_history_commands::play_history_list,
            commands::play_history_commands::play_history_stats,
            // favorites (Phase 11)
            commands::favorite_commands::favorites_favorite,
            commands::favorite_commands::favorites_unfavorite,
            commands::favorite_commands::favorites_is_favorite,
            commands::favorite_commands::favorites_list_tracks,
            commands::favorite_commands::favorites_list_albums,
            commands::favorite_commands::favorites_list_artists,
            commands::favorite_commands::favorites_track_ids,
            // discover (Phase 11) + acoustic similarity (Phase 12)
            commands::discover_commands::discover_home,
            commands::discover_commands::discover_radio,
            commands::discover_commands::discover_similar,
            commands::discover_commands::discover_playlist_recommendations,
            commands::discover_commands::fingerprint_status,
            // lyrics (Phase 15)
            commands::lyrics_commands::get_lyrics,
            commands::lyrics_commands::refetch_lyrics,
            commands::lyrics_commands::set_lyrics,
            commands::lyrics_commands::clear_lyrics,
            // discography sync (Phase 14)
            commands::discography_commands::discography_report,
            commands::discography_commands::discography_sync,
            commands::discography_commands::discography_candidates,
            commands::discography_commands::discography_resolve,
            commands::discography_commands::discography_ignores,
            commands::discography_commands::discography_add_ignore,
            commands::discography_commands::discography_remove_ignore,
            commands::discography_commands::discography_status,
            commands::discography_commands::discography_sync_all,
            // podcasts
            commands::podcast_commands::podcast_search,
            commands::podcast_commands::podcast_list,
            commands::podcast_commands::podcast_get,
            commands::podcast_commands::podcast_list_episodes,
            commands::podcast_commands::podcast_record_progress,
            commands::podcast_commands::podcast_subscribe_feed,
            commands::podcast_commands::podcast_subscribe,
            commands::podcast_commands::podcast_unsubscribe,
            commands::podcast_commands::podcast_refresh,
            commands::podcast_commands::podcast_set_auto_download,
            // background notification poll (Android WorkManager; no-op on desktop)
            notify_sync::notif_background_sync_enable,
            notify_sync::notif_background_sync_disable,
            // real-time push registration (Android FCM; no-op on desktop)
            push::push_register,
            push::push_unregister,
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
            commands::player_commands::player_prefetch,
            commands::player_commands::player_prefetch_is_ready,
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
            commands::download_commands::podcast_download_episode,
            commands::download_commands::podcast_download_show,
            commands::download_commands::podcast_delete_episode,
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
            commands::upload_commands::uploads_set_concurrency,
            commands::upload_commands::uploads_resume_pending,
            commands::upload_commands::uploads_subscribe,
            // Android "All files access" (MANAGE_EXTERNAL_STORAGE) check + request.
            upload_session::storage_has_all_files_access,
            upload_session::storage_request_all_files_access,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
