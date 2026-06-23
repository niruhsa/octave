//! Native media-session bridge — Android system media notification + lock screen.
//!
//! ## Why this is native (and the Web Media Session API isn't enough)
//!
//! Playback runs in the webview (`<audio>` + the in-app loopback server). A
//! full browser like Chrome surfaces `navigator.mediaSession` to Android's
//! system media notification, but a **bare embedded `WebView`** (what Tauri/wry
//! uses) does not. So there is no shade/lock-screen notification unless the
//! *native* side owns a [`MediaSessionCompat`] and posts a foreground
//! `MediaStyle` notification.
//!
//! ## The bridge
//!
//! This is a small app-inline Tauri plugin bound to the Kotlin
//! `MediaSessionPlugin` via [`register_android_plugin`]. The flow is
//! bidirectional:
//!
//! * **JS → native**: on track / play-pause / seek changes the frontend calls
//!   [`media_session_update`] / [`media_session_set_playback`] /
//!   [`media_session_clear`], which `run_mobile_plugin` into the Kotlin plugin
//!   to update the session + notification. Album art is passed as a loopback
//!   URL (`player_cover_url`) the Kotlin side fetches as a bitmap.
//! * **native → JS**: notification / lock-screen / Bluetooth transport buttons
//!   fire the `MediaSession` callbacks, which the Kotlin plugin re-emits as
//!   `media-session`-plugin `action` events; the frontend turns those into
//!   player-store actions (see `useNativeMediaSession`).
//!
//! Everything is **Android-only**; on desktop the commands are no-ops (the
//! native handle is never bound) and the existing Web Media Session keeps
//! driving macOS Now Playing / Windows SMTC.

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{Manager, Runtime};

/// Full now-playing state pushed on a track change (and play/pause).
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Loopback cover URL (`player_cover_url`) or `null` when art is unknown.
    pub artwork_url: Option<String>,
    /// Loopback base (`player_action_url_base`) the native side posts transport
    /// presses to. Carried on every update so a freshly-bound session has it.
    pub action_base_url: String,
    pub duration_ms: i64,
    pub position_ms: i64,
    pub playing: bool,
}

/// Lightweight update (play/pause, seek, periodic position correction). The
/// system extrapolates the scrubber from `position + elapsed`, so this is sent
/// only on state changes + an occasional heartbeat, not every tick.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackInfo {
    pub position_ms: i64,
    pub duration_ms: i64,
    pub playing: bool,
}

/// Holds the bound Android plugin handle. `None` on desktop / before setup.
pub struct MediaSessionHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

/// Register the bridge plugin. On Android it binds the Kotlin
/// `MediaSessionPlugin`; elsewhere it just manages an empty handle so the
/// commands compile + no-op.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("media-session")
        .setup(|app, _api| {
            let handle: Option<PluginHandle<R>> = {
                #[cfg(target_os = "android")]
                {
                    Some(_api.register_android_plugin(
                        "dev.niruhsa.music.app",
                        "MediaSessionPlugin",
                    )?)
                }
                #[cfg(not(target_os = "android"))]
                {
                    None
                }
            };
            app.manage(MediaSessionHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

fn run<R: Runtime>(
    handle: &MediaSessionHandle<R>,
    command: &str,
    payload: impl Serialize,
) -> Result<(), String> {
    let guard = handle
        .0
        .lock()
        .map_err(|_| "media-session handle poisoned".to_string())?;
    match guard.as_ref() {
        Some(_h) => {
            // `run_mobile_plugin` only exists on mobile targets; on desktop the
            // handle is always `None` so this arm is unreachable there anyway.
            #[cfg(mobile)]
            {
                // `serde_json::Value` accepts whatever the Kotlin
                // `invoke.resolve()` returns (`{}` / `null`) without erroring.
                _h.run_mobile_plugin::<serde_json::Value>(command, payload)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            #[cfg(not(mobile))]
            {
                let _ = (command, payload);
                Ok(())
            }
        }
        None => Ok(()),
    }
}

// The commands take `AppHandle<R>` (rather than `State<MediaSessionHandle<R>>`)
// so the runtime type `R` is pinned — a command whose only runtime-generic
// reference is the state arg can't infer `R`.

#[tauri::command]
pub fn media_session_update<R: Runtime>(
    app: tauri::AppHandle<R>,
    info: MediaInfo,
) -> Result<(), String> {
    run(app.state::<MediaSessionHandle<R>>().inner(), "update", info)
}

#[tauri::command]
pub fn media_session_set_playback<R: Runtime>(
    app: tauri::AppHandle<R>,
    info: PlaybackInfo,
) -> Result<(), String> {
    run(app.state::<MediaSessionHandle<R>>().inner(), "setPlayback", info)
}

#[tauri::command]
pub fn media_session_clear<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    run(
        app.state::<MediaSessionHandle<R>>().inner(),
        "clear",
        serde_json::json!({}),
    )
}
