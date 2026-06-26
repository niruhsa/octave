//! Native Android **background notification poll** bridge (Phase 10).
//!
//! ## Why this exists
//!
//! The in-app notification poller ([`crate::commands::notification_commands`] +
//! the JS `useNotificationsScheduler`) only runs while the app is open. Once
//! Android kills the process (swiped from recents) — or even just backgrounds
//! it — that polling stops, so new-release notifications stop arriving. Apps
//! that notify while closed use OS **push** (FCM/APNs); the lighter,
//! no-Firebase alternative is a periodic background job.
//!
//! ## The bridge
//!
//! A small app-inline Tauri plugin bound to the Kotlin `NotificationSyncPlugin`
//! via [`register_android_plugin`], mirroring [`crate::upload_session`]. The
//! Kotlin side schedules a `WorkManager` [`NotificationPollWorker`] that wakes
//! ~every 15 min (independent of the app process), calls the existing
//! `GET /notifications?unread=true` endpoint itself, and posts a system
//! notification per new unread row.
//!
//! The worker is headless Kotlin (the Rust core / WebView aren't running when
//! the app is closed), so it needs the server base URL + bearer token up front.
//! [`notif_background_sync_enable`] reads those from the secure credential store
//! **in Rust** and hands them to the plugin — the token never passes through the
//! WebView/JS layer. Everything is Android-only; desktop / iOS calls no-op.

use serde::Serialize;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, State};

use crate::error::AppResult;

/// Holds the bound Android plugin handle. `None` on desktop / before setup.
pub struct NotifySyncHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

/// `start` payload — the server base URL + bearer token the worker polls with.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
struct StartArgs<'a> {
    base_url: &'a str,
    token: &'a str,
}

/// Register the bridge plugin. On Android it binds the Kotlin
/// `NotificationSyncPlugin`; elsewhere it just manages an empty handle so the
/// helpers compile + no-op.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("notify-sync")
        .setup(|app, _api| {
            let handle: Option<PluginHandle<R>> = {
                #[cfg(target_os = "android")]
                {
                    Some(_api.register_android_plugin(
                        "dev.niruhsa.octave",
                        "NotificationSyncPlugin",
                    )?)
                }
                #[cfg(not(target_os = "android"))]
                {
                    None
                }
            };
            app.manage(NotifySyncHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

fn run<R: Runtime>(app: &AppHandle<R>, command: &str, payload: impl Serialize) {
    let state = app.state::<NotifySyncHandle<R>>();
    let guard = match state.0.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(_h) = guard.as_ref() {
        #[cfg(mobile)]
        {
            if let Err(e) = _h.run_mobile_plugin::<serde_json::Value>(command, payload) {
                tracing::warn!(command, error = %e, "notify-sync plugin call failed");
            }
        }
        #[cfg(not(mobile))]
        {
            let _ = (command, payload);
        }
    }
}

/// Enable the background notification poll for the current user.
///
/// Reads the active **bearer** token + REST base from the secure credential
/// store (never via the WebView) and hands them to the Kotlin worker. A
/// `SECRET_KEY` session (no per-user notifications) or no session disables it
/// instead. No-op on desktop / iOS. Idempotent — safe to call on every session
/// change (it re-pushes the current token, e.g. after a re-login).
#[tauri::command]
pub async fn notif_background_sync_enable<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::AppStateHandle>,
) -> AppResult<()> {
    #[cfg(target_os = "android")]
    {
        let manager = { state.auth.read().await.as_ref().cloned() };
        let Some(manager) = manager else {
            run(&app, "stop", serde_json::json!({}));
            return Ok(());
        };
        match manager.credential().await {
            Ok(crate::transport::Credential::Bearer(token)) => {
                let base = manager.server_config().rest_url;
                run(
                    &app,
                    "start",
                    StartArgs { base_url: base.as_str(), token: token.as_str() },
                );
            }
            // SECRET_KEY (no per-user notifications) or no credential — disable.
            _ => run(&app, "stop", serde_json::json!({})),
        }
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (&app, &state);
        Ok(())
    }
}

/// Disable the background notification poll (logout / no eligible user).
/// No-op on desktop / iOS.
#[tauri::command]
pub fn notif_background_sync_disable<R: Runtime>(app: AppHandle<R>) -> AppResult<()> {
    run(&app, "stop", serde_json::json!({}));
    Ok(())
}
