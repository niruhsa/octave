//! Native Android **FCM push** bridge (Phase 10 — real-time notifications).
//!
//! Firebase Cloud Messaging delivers new-release notifications instantly, even
//! when the app is swiped away — the OS displays the server's notification
//! message without the app process running. This bridge (mirroring
//! [`crate::notify_sync`] / [`crate::upload_session`]) binds the Kotlin
//! `PushPlugin` to:
//!   * fetch the device's FCM registration token, and
//!   * register it with the server so the new-release fan-out can target it.
//!
//! The bearer token used to authorize registration is read from the secure
//! store **in Rust** (via [`AuthManager`](crate::auth::AuthManager)), so it
//! never passes through the WebView. Everything is Android-only; desktop / iOS
//! calls no-op (`push_register` returns `false`, so the caller falls back to
//! the WorkManager poll / foreground polling).

use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, State};

use crate::error::AppResult;

/// Holds the bound Android plugin handle. `None` on desktop / before setup.
pub struct PushHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

/// Register the bridge plugin. On Android it binds the Kotlin `PushPlugin`;
/// elsewhere it manages an empty handle so the commands compile + no-op.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("push")
        .setup(|app, _api| {
            let handle: Option<PluginHandle<R>> = {
                #[cfg(target_os = "android")]
                {
                    Some(_api.register_android_plugin("dev.niruhsa.octave", "PushPlugin")?)
                }
                #[cfg(not(target_os = "android"))]
                {
                    None
                }
            };
            app.manage(PushHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

/// Fetch the current FCM token from Kotlin. `None` on desktop / when Google
/// Play Services is unavailable (so the caller uses the WorkManager fallback).
#[cfg(target_os = "android")]
fn fcm_token<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct TokenReply {
        token: String,
    }
    let state = app.state::<PushHandle<R>>();
    let guard = state.0.lock().ok()?;
    let h = guard.as_ref()?;
    let reply: TokenReply = h.run_mobile_plugin("getToken", ()).ok()?;
    let t = reply.token.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

#[cfg(target_os = "android")]
fn delete_fcm_token<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<PushHandle<R>>();
    if let Ok(guard) = state.0.lock() {
        if let Some(h) = guard.as_ref() {
            let _ = h.run_mobile_plugin::<serde_json::Value>("deleteToken", ());
        }
    }
}

/// Register this device for FCM push: fetch the token + register it with the
/// server. Returns `true` when FCM is available and the token was registered;
/// `false` on desktop / no Play Services / no bearer session — the caller then
/// falls back to the WorkManager background poll.
#[tauri::command]
pub async fn push_register<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::AppStateHandle>,
) -> AppResult<bool> {
    #[cfg(target_os = "android")]
    {
        let Some(token) = fcm_token(&app) else {
            return Ok(false);
        };
        let manager = { state.auth.read().await.as_ref().cloned() };
        let Some(manager) = manager else {
            return Ok(false);
        };
        // Only a bearer user can own a device token (the server rejects
        // SECRET_KEY); a failure here just means we fall back to polling.
        match manager.register_device(&token, "android").await {
            Ok(()) => Ok(true),
            Err(e) => {
                tracing::warn!(error = %e, "push_register: server register_device failed");
                Ok(false)
            }
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (&app, &state);
        Ok(false)
    }
}

/// Unregister this device (logout): tell the server to drop the token, then
/// delete it locally so the OS stops receiving pushes for the signed-out user.
/// Call this **before** clearing the session (registration needs the bearer
/// credential). No-op on desktop.
#[tauri::command]
pub async fn push_unregister<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, crate::AppStateHandle>,
) -> AppResult<()> {
    #[cfg(target_os = "android")]
    {
        if let Some(token) = fcm_token(&app) {
            let manager = { state.auth.read().await.as_ref().cloned() };
            if let Some(manager) = manager {
                if let Err(e) = manager.unregister_device(&token).await {
                    tracing::warn!(error = %e, "push_unregister: server unregister failed");
                }
            }
        }
        delete_fcm_token(&app);
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (&app, &state);
        Ok(())
    }
}
