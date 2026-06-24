//! Native Android **foreground-service** bridge for background uploads.
//!
//! ## Why this exists
//!
//! The upload job ([`commands::upload_commands`]) runs entirely in Rust — a
//! spawned tokio task reads each file natively, hashes it, and drives the
//! chunked gRPC/REST transfer. None of that touches the WebView, so a paused
//! WebView is not the problem.
//!
//! The problem is that, with **no foreground service**, Android applies its
//! background network restriction + Doze the moment the app loses foreground
//! (backgrounded, or the screen locked / turned off). That severs the in-flight
//! request and the upload fails almost instantly. An `.ongoing()` notification
//! is **not** a foreground service — it just can't be swiped away; it grants the
//! process no background-execution privilege at all.
//!
//! So, exactly like the media notification ([`media_session`]), the fix is
//! native: a Kotlin `UploadService` (a `dataSync` foreground service that also
//! holds a partial wake lock + WiFi lock) keeps the process — and its network —
//! alive while the upload runs, behind a persistent notification.
//!
//! ## The bridge
//!
//! This is a small app-inline Tauri plugin bound to the Kotlin
//! `UploadServicePlugin` via [`register_android_plugin`]. The flow is one-way
//! (Rust → native): the upload job calls [`start`] when it begins, [`update`]
//! per progress tick (throttled), and [`stop`] when it ends (any exit path).
//!
//! Everything is **Android-only**; on desktop / iOS the handle is never bound
//! and every call is a no-op.

use serde::Serialize;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};

/// Holds the bound Android plugin handle. `None` on desktop / before setup.
pub struct UploadSessionHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

/// Foreground-service notification state pushed on start / update.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForegroundArgs<'a> {
    title: &'a str,
    body: &'a str,
    /// `0..=100` → determinate progress bar; `< 0` → indeterminate.
    progress: i32,
}

/// Register the bridge plugin. On Android it binds the Kotlin
/// `UploadServicePlugin`; elsewhere it just manages an empty handle so the
/// helpers compile + no-op.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("upload-session")
        .setup(|app, _api| {
            let handle: Option<PluginHandle<R>> = {
                #[cfg(target_os = "android")]
                {
                    Some(_api.register_android_plugin("dev.niruhsa.octave", "UploadServicePlugin")?)
                }
                #[cfg(not(target_os = "android"))]
                {
                    None
                }
            };
            app.manage(UploadSessionHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

fn run<R: Runtime>(app: &AppHandle<R>, command: &str, payload: impl Serialize) {
    let state = app.state::<UploadSessionHandle<R>>();
    let guard = match state.0.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(_h) = guard.as_ref() {
        // `run_mobile_plugin` only exists on mobile targets; on desktop the
        // handle is always `None` so this arm is unreachable there anyway.
        #[cfg(mobile)]
        {
            // The Kotlin `invoke.resolve()` returns `{}` / `null`, which
            // `serde_json::Value` accepts without erroring.
            if let Err(e) = _h.run_mobile_plugin::<serde_json::Value>(command, payload) {
                tracing::warn!(command, error = %e, "upload-session plugin call failed");
            }
        }
        #[cfg(not(mobile))]
        {
            let _ = (command, payload);
        }
    }
}

/// Start the upload foreground service (Android). No-op on desktop / iOS.
pub fn start<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str, progress: i32) {
    run(
        app,
        "start",
        ForegroundArgs {
            title,
            body,
            progress,
        },
    );
}

/// Update the foreground-service notification text / progress. No-op on desktop.
pub fn update<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str, progress: i32) {
    run(
        app,
        "update",
        ForegroundArgs {
            title,
            body,
            progress,
        },
    );
}

/// Stop the upload foreground service + release its wake / WiFi locks. No-op on
/// desktop. Safe to call from any job exit path (including a panic-unwind drop).
pub fn stop<R: Runtime>(app: &AppHandle<R>) {
    run(app, "stop", serde_json::json!({}));
}

/// Take **persistable** read permission on the given `content://` URIs (Android)
/// so they stay readable after a process kill — required to resume an upload
/// from disk. No-op on desktop / for non-content paths (the Kotlin side filters
/// by scheme and swallows non-persistable grants).
pub fn persist_uri_access<R: Runtime>(app: &AppHandle<R>, uris: Vec<String>) {
    run(app, "persistUriPermissions", UriArgs { uris });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UriArgs {
    uris: Vec<String>,
}

/// Call a plugin command that returns a value, deserializing it. Android-only
/// (the only caller, the all-files-access check, is gated to Android).
#[cfg(target_os = "android")]
fn run_get<R: Runtime, T: serde::de::DeserializeOwned>(
    app: &AppHandle<R>,
    command: &str,
) -> Option<T> {
    let state = app.state::<UploadSessionHandle<R>>();
    let guard = state.0.lock().ok()?;
    let h = guard.as_ref()?;
    h.run_mobile_plugin::<T>(command, ()).ok()
}

/// Whether the app holds full **"All files access"** on Android
/// (`MANAGE_EXTERNAL_STORAGE`, granted from a system settings screen). Always
/// `true` on desktop (the process already has full filesystem access) and on
/// Android ≤10 (no such mode; legacy read grants apply).
#[tauri::command]
pub fn storage_has_all_files_access<R: Runtime>(app: AppHandle<R>) -> bool {
    #[cfg(target_os = "android")]
    {
        #[derive(serde::Deserialize)]
        struct Granted {
            granted: bool,
        }
        run_get::<R, Granted>(&app, "hasAllFilesAccess")
            .map(|g| g.granted)
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        true
    }
}

/// Open the system **"All files access"** settings screen so the user can grant
/// `MANAGE_EXTERNAL_STORAGE` (it can't be granted by a normal runtime dialog).
/// No-op on desktop / when already granted.
#[tauri::command]
pub fn storage_request_all_files_access<R: Runtime>(app: AppHandle<R>) {
    run(&app, "requestAllFilesAccess", serde_json::json!({}));
}
