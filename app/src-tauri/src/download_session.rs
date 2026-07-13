//! Native Android **foreground-service** bridge for background downloads.
//!
//! The mirror image of [`crate::upload_session`]. Offline downloads run entirely
//! in Rust (a tokio task streaming each track file — see [`crate::downloads`]),
//! so a paused WebView is not the problem. The problem is that with **no
//! foreground service** Android applies its background-network restriction +
//! Doze the moment the app loses foreground (backgrounded, or the screen locked
//! / off), which severs the in-flight transfer and fails the download.
//!
//! So, exactly like uploads, the fix is native: a Kotlin `DownloadService` (a
//! `dataSync` foreground service that also holds a partial wake lock + WiFi lock)
//! keeps the process — and its network — alive while a download runs, behind a
//! persistent notification.
//!
//! This is a small app-inline Tauri plugin bound to the Kotlin
//! `DownloadServicePlugin`. The flow is one-way (Rust → native): the download job
//! calls [`start`] when it begins, [`update`] per progress tick, and [`stop`]
//! when it ends (any exit path — use [`ForegroundGuard`]).
//!
//! Everything is **Android-only**; on desktop / iOS the handle is never bound and
//! every call is a no-op.

use serde::Serialize;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};

/// Holds the bound Android plugin handle. `None` on desktop / before setup.
pub struct DownloadSessionHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

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
/// `DownloadServicePlugin`; elsewhere it just manages an empty handle so the
/// helpers compile + no-op.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("download-session")
        .setup(|app, _api| {
            let handle: Option<PluginHandle<R>> =
                {
                    #[cfg(target_os = "android")]
                    {
                        Some(_api.register_android_plugin(
                            "dev.niruhsa.octave",
                            "DownloadServicePlugin",
                        )?)
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        None
                    }
                };
            app.manage(DownloadSessionHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

fn run<R: Runtime>(app: &AppHandle<R>, command: &str, payload: impl Serialize) {
    let state = app.state::<DownloadSessionHandle<R>>();
    let guard = match state.0.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(_h) = guard.as_ref() {
        // `run_mobile_plugin` only exists on mobile targets; on desktop the
        // handle is always `None` so this arm is unreachable there anyway.
        #[cfg(mobile)]
        {
            if let Err(e) = _h.run_mobile_plugin::<serde_json::Value>(command, payload) {
                tracing::warn!(command, error = %e, "download-session plugin call failed");
            }
        }
        #[cfg(not(mobile))]
        {
            let _ = (command, payload);
        }
    }
}

/// Start the download foreground service (Android). No-op on desktop / iOS.
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

/// Stop the download foreground service + release its wake / WiFi locks. No-op on
/// desktop. Safe to call from any job exit path (including a panic-unwind drop).
pub fn stop<R: Runtime>(app: &AppHandle<R>) {
    run(app, "stop", serde_json::json!({}));
}

/// RAII guard that stops the Android download foreground service (releasing its
/// wake / WiFi locks + persistent notification) when the job ends — on **any**
/// exit path, including an early `return` or an error `?`. No-op on desktop.
pub struct ForegroundGuard<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> ForegroundGuard<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> Drop for ForegroundGuard<R> {
    fn drop(&mut self) {
        stop(&self.app);
    }
}
