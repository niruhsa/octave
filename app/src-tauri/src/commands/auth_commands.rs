//! Tauri commands exposing auth + transport state to the frontend.
//!
//! The frontend tells us which server to talk to (`auth_configure_server`),
//! then either logs in with a username/password or installs a `SECRET_KEY`.
//! From there, `auth_session` returns the current snapshot and `auth_logout`
//! clears it.

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::auth::store::SecureStore;
use crate::auth::{AuthManager, AuthSession};
use crate::error::{AppError, AppResult};
use crate::transport::{ServerClient, ServerConfig};
use crate::AppStateHandle;

#[cfg(target_os = "android")]
const CRED_FILENAME: &str = "credential.json";

/// Install (or replace) the active server config. Builds a fresh
/// `AuthManager` keyed to it. Any previously stored credential is loaded;
/// the user does NOT need to re-log-in if the same server is reconfigured.
///
/// `rest_url` is the URL the user knows (the one they can `curl`). If
/// `grpc_url` is `None` we derive it: dev port `8080` → `50051`,
/// everything else stays put (assumed reverse-proxied).
#[tauri::command]
pub async fn auth_configure_server(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    rest_url: String,
    grpc_url: Option<String>,
) -> AppResult<()> {
    let config = match grpc_url.as_deref() {
        Some(g) if !g.trim().is_empty() => ServerConfig::new(&rest_url, g)?,
        _ => ServerConfig::from_rest_only(&rest_url)?,
    };
    tracing::info!(rest = %config.rest_root(), grpc = %config.grpc_endpoint(), "configuring server");
    let server = Arc::new(ServerClient::new(config)?);
    let store = build_store(&app)?;
    let manager = Arc::new(AuthManager::new(server, store));
    manager.restore_from_store().await;
    // Touch /health once so the cached online flag is fresh for the UI.
    let _ = manager.refresh_online().await;
    *state.auth.write().await = Some(manager);
    Ok(())
}

/// Username/password login. Persists the bearer token and returns the
/// resulting session for the UI to render.
#[tauri::command]
pub async fn auth_login(
    state: State<'_, AppStateHandle>,
    username: String,
    password: String,
) -> AppResult<AuthSession> {
    with_manager(&state, |m| async move { m.login(&username, &password).await }).await
}

/// Install a `SECRET_KEY` credential. Verified server-side via `WhoAmI`
/// before being persisted.
#[tauri::command]
pub async fn auth_set_secret_key(
    state: State<'_, AppStateHandle>,
    secret_key: String,
) -> AppResult<AuthSession> {
    with_manager(&state, |m| async move { m.set_secret_key(&secret_key).await }).await
}

/// Resolve the current credential against the server. Returns the live tier.
#[tauri::command]
pub async fn auth_whoami(state: State<'_, AppStateHandle>) -> AppResult<AuthSession> {
    with_manager(&state, |m| async move { m.whoami().await }).await
}

/// Return the cached session snapshot WITHOUT touching the server. Used by
/// the UI on boot for an instant render before any network roundtrip.
#[tauri::command]
pub async fn auth_session(state: State<'_, AppStateHandle>) -> AppResult<Option<AuthSession>> {
    let guard = state.auth.read().await;
    match guard.as_ref() {
        Some(m) => Ok(m.current().await),
        None => Ok(None),
    }
}

/// Log out and wipe stored credentials.
#[tauri::command]
pub async fn auth_logout(state: State<'_, AppStateHandle>) -> AppResult<()> {
    with_manager(&state, |m| async move { m.logout().await }).await
}

/// Run a `/health` ping and return whether the server is reachable.
#[tauri::command]
pub async fn auth_refresh_online(state: State<'_, AppStateHandle>) -> AppResult<bool> {
    with_manager(&state, |m| async move { Ok(m.refresh_online().await) }).await
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Borrow the active `AuthManager` (cloned `Arc`) and run a closure.
/// Returns `AuthNotConfigured` if no server URL has been set yet.
async fn with_manager<F, Fut, T>(state: &State<'_, AppStateHandle>, f: F) -> AppResult<T>
where
    F: FnOnce(Arc<AuthManager>) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let manager = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    f(manager).await
}

/// Build the platform-appropriate secure store.
///
/// On desktop we prefer the OS keychain; on Android we fall back to a file
/// in app-private storage. We default to the keychain on every desktop
/// platform — `keyring::Entry::new` is cheap, real failures surface on
/// first `load/save`.
#[cfg(not(target_os = "android"))]
fn build_store(_app: &AppHandle) -> AppResult<Arc<dyn SecureStore>> {
    Ok(Arc::new(crate::auth::store::KeyringStore::new()))
}

#[cfg(target_os = "android")]
fn build_store(app: &AppHandle) -> AppResult<Arc<dyn SecureStore>> {
    use tauri::Manager;
    // App-private dir on Android (scoped storage). File lives next to the
    // cache DB.
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::SecureStorage(format!("resolve app_data_dir: {e}")))?;
    let path = dir.join(CRED_FILENAME);
    Ok(Arc::new(crate::auth::store::FileStore::new(path)))
}
