//! Platform audio-output discovery for automatic EQ profile selection.
//!
//! Native endpoint identifiers are intentionally kept in [`AudioOutput`]
//! inside Rust. The command/service layer must expose only the redacted output
//! summary; exact bindings persist an HMAC-derived key, never `runtime_id`.

use std::future::Future;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::sync::Arc;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::time::Duration;

use tauri::{AppHandle, Runtime};

use super::model::AudioOutput;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Register Android's inline route plugin. Other targets receive an inert
/// plugin so the Tauri builder can use one unconditional registration call.
pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    android_bridge::init()
}

/// Query all currently known output candidates.
///
/// On desktop this is a synchronous OS call moved to a blocking worker. On
/// Android it invokes the Kotlin plugin, which owns `AudioManager`.
pub async fn query_outputs<R: Runtime>(app: AppHandle<R>) -> Result<Vec<AudioOutput>, String> {
    #[cfg(target_os = "android")]
    {
        android::query_outputs(&app)
    }

    #[cfg(target_os = "windows")]
    {
        let _ = app;
        tauri::async_runtime::spawn_blocking(windows::query_outputs)
            .await
            .map_err(|e| format!("join Windows output query: {e}"))?
    }

    #[cfg(target_os = "macos")]
    {
        let _ = app;
        tauri::async_runtime::spawn_blocking(macos::query_outputs)
            .await
            .map_err(|e| format!("join macOS output query: {e}"))?
    }

    #[cfg(not(any(target_os = "android", target_os = "windows", target_os = "macos")))]
    {
        let _ = app;
        Ok(Vec::new())
    }
}

/// Supply Android's plugin with the token-authenticated loopback callback.
/// This is called only after the media server has been bound and managed.
pub fn configure_callback<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    android_bridge::configure_callback(app)
}

/// Monitor desktop default-output changes without blocking playback.
///
/// Windows Core Audio/macOS CoreAudio queries are cheap and run off the async
/// worker. A new
/// descriptor must remain stable for 650 ms before publication, which absorbs
/// the burst of add/default/property events common during Bluetooth hand-off.
/// Android uses its native `AudioDeviceCallback` signal instead and therefore
/// does not start this polling fallback.
pub fn spawn_monitor<R, F, Fut>(app: AppHandle<R>, on_change: F)
where
    R: Runtime,
    F: Fn(AppHandle<R>, Vec<AudioOutput>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let callback = Arc::new(on_change);
        let (change_tx, mut change_rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(target_os = "windows")]
        windows::spawn_change_listener(change_tx);
        #[cfg(target_os = "macos")]
        macos::spawn_change_listener(change_tx);
        tauri::async_runtime::spawn(async move {
            let mut published: Option<Vec<AudioOutput>> = None;
            loop {
                if published.is_some() {
                    // Native events are the normal path. The slow fallback
                    // catches listener-registration failure, resume races, and
                    // endpoint stacks that omit a property notification.
                    tokio::select! {
                        _ = change_rx.recv() => {
                            while change_rx.try_recv().is_ok() {}
                        }
                        _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                    }
                }
                match query_outputs(app.clone()).await {
                    Ok(candidate) if published.as_ref() != Some(&candidate) => {
                        let confirmed = if published.is_some() {
                            tokio::time::sleep(Duration::from_millis(650)).await;
                            query_outputs(app.clone()).await
                        } else {
                            Ok(candidate.clone())
                        };
                        match confirmed {
                            Ok(value) if value == candidate => {
                                published = Some(value.clone());
                                callback(app.clone(), value).await;
                            }
                            Ok(_) => {
                                // Still settling. The next iteration observes
                                // the latest candidate without publishing a flap.
                            }
                            Err(error) => tracing::debug!(%error, "confirm audio output failed"),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => tracing::debug!(%error, "query audio output failed"),
                }
            }
        });
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (app, on_change);
    }
}

// Keep an inert implementation compiled everywhere so `lib.rs` can register
// the plugin unconditionally. The real module is re-exported on Android.
#[cfg(target_os = "android")]
use android as android_bridge;

#[cfg(not(target_os = "android"))]
mod android_bridge {
    use tauri::plugin::{Builder, TauriPlugin};
    use tauri::{AppHandle, Runtime};

    pub fn init<R: Runtime>() -> TauriPlugin<R> {
        Builder::new("audio-route").build()
    }

    pub fn configure_callback<R: Runtime>(_app: &AppHandle<R>) -> Result<(), String> {
        Ok(())
    }
}
