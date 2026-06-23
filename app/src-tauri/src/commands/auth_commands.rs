//! Tauri commands exposing auth + transport state to the frontend.
//!
//! The frontend tells us which server to talk to (`auth_configure_server`),
//! then either logs in with a username/password or installs a `SECRET_KEY`.
//! From there, `auth_session` returns the current snapshot and `auth_logout`
//! clears it.

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::auth::store::{SecureStore, StoredCredentialKind};
use crate::auth::{AuthManager, AuthSession};
use crate::error::{AppError, AppResult};
use crate::transport::{PermissionTier, ServerClient, ServerConfig};
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

/// The server URLs the client is currently pointed at. Used to pre-fill the
/// in-app "change server" form. `None` when no server is configured yet.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerInfo {
    pub rest_url: String,
    pub grpc_url: String,
    /// `true` when `grpc_url` is a user override (vs auto-derived from REST).
    /// The UI prefills + reveals the gRPC field only for explicit overrides.
    pub grpc_explicit: bool,
}

/// Return the active server config (REST + derived/explicit gRPC URL).
#[tauri::command]
pub async fn auth_server_config(state: State<'_, AppStateHandle>) -> AppResult<Option<ServerInfo>> {
    let guard = state.auth.read().await;
    Ok(guard.as_ref().map(|m| {
        let c = m.server_config();
        ServerInfo {
            rest_url: c.rest_url,
            grpc_url: c.grpc_url,
            grpc_explicit: c.grpc_explicit,
        }
    }))
}

/// Re-point the app at a (possibly different) server **while signed in**.
///
/// Builds a fresh client for the new URLs, then validates the stored
/// credential against it: returns the live `AuthSession` when the token is
/// still accepted (and persists the new URLs so a restart reconnects there),
/// or `None` when the credential is rejected on the new server (the UI should
/// route to login). If the new server is unreachable the session is kept
/// optimistically with the new URLs.
#[tauri::command]
pub async fn auth_change_server(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
    rest_url: String,
    grpc_url: Option<String>,
) -> AppResult<Option<AuthSession>> {
    let config = match grpc_url.as_deref() {
        Some(g) if !g.trim().is_empty() => ServerConfig::new(&rest_url, g)?,
        _ => ServerConfig::from_rest_only(&rest_url)?,
    };
    tracing::info!(rest = %config.rest_root(), grpc = %config.grpc_endpoint(), "changing server");
    let server = Arc::new(ServerClient::new(config)?);
    let store = build_store(&app)?;
    let manager = Arc::new(AuthManager::new(server, store));
    manager.restore_from_store().await;
    let session = manager.revalidate_for_new_server().await?;
    *state.auth.write().await = Some(manager);
    Ok(session)
}

/// Username/password login. Persists the bearer token and returns the
/// resulting session for the UI to render.
#[tauri::command]
pub async fn auth_login(
    state: State<'_, AppStateHandle>,
    username: String,
    password: String,
) -> AppResult<AuthSession> {
    with_manager(
        &state,
        |m| async move { m.login(&username, &password).await },
    )
    .await
}

/// Install a `SECRET_KEY` credential. Verified server-side via `WhoAmI`
/// before being persisted.
#[tauri::command]
pub async fn auth_set_secret_key(
    state: State<'_, AppStateHandle>,
    secret_key: String,
) -> AppResult<AuthSession> {
    with_manager(
        &state,
        |m| async move { m.set_secret_key(&secret_key).await },
    )
    .await
}

/// Resolve the current credential against the server. Returns the live tier.
#[tauri::command]
pub async fn auth_whoami(state: State<'_, AppStateHandle>) -> AppResult<AuthSession> {
    with_manager(&state, |m| async move { m.whoami().await }).await
}

/// Return the cached session snapshot WITHOUT touching the server. Used by
/// the UI on boot for an instant render before any network roundtrip.
///
/// When no manager is configured yet (fresh boot, Tauri state is empty)
/// but a Bearer credential is persisted in the platform's secure store
/// (keychain / app-private file) *with* server URLs — i.e. the user
/// logged in with a username+password in a prior session — this command
/// lazily rebuilds the `AuthManager` from the stored config so the
/// session survives an app restart without the user re-entering the
/// server address.
///
/// `SECRET_KEY` entries are NOT auto-restored — the user must re-enter
/// the key each session (the directive is "only applicable to user
/// accounts, not secret-key login").
#[tauri::command]
pub async fn auth_session(
    app: AppHandle,
    state: State<'_, AppStateHandle>,
) -> AppResult<Option<AuthSession>> {
    {
        let guard = state.auth.read().await;
        if let Some(ref m) = *guard {
            return Ok(m.current().await);
        }
    }
    // No manager yet — try to rebuild from the persisted credential.
    let store = build_store(&app)?;
    let stored = match store.load().await {
        Ok(Some(s)) => s,
        _ => return Ok(None),
    };
    // Only auto-restore Bearer (username+password) sessions.
    if !matches!(stored.kind, StoredCredentialKind::Bearer) {
        return Ok(None);
    }
    let rest_url = match stored.rest_url.as_deref() {
        Some(u) if !u.is_empty() => u,
        _ => return Ok(None),
    };
    // Honour an explicit gRPC override; otherwise re-derive from REST so a
    // derived URL keeps tracking the REST URL (legacy entries lack the flag →
    // treated as derived).
    let config = match stored.grpc_url.as_deref().filter(|g| !g.is_empty()) {
        Some(grpc) if stored.grpc_explicit.unwrap_or(false) => ServerConfig::new(rest_url, grpc)?,
        _ => ServerConfig::from_rest_only(rest_url)?,
    };
    let server = Arc::new(ServerClient::new(config)?);
    let manager = Arc::new(AuthManager::new(server, store));
    manager.restore_from_store().await;
    let session = manager.current().await;
    *state.auth.write().await = Some(manager);
    Ok(session)
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

/// Probe both transports (gRPC primary + REST fallback) concurrently and
/// return which are reachable. Drives the per-transport status indicator;
/// `rest || grpc` is the overall online state.
#[tauri::command]
pub async fn auth_refresh_transports(
    state: State<'_, AppStateHandle>,
) -> AppResult<crate::transport::TransportHealth> {
    with_manager(&state, |m| async move { Ok(m.refresh_transports().await) }).await
}

/// Register a new account. Server-gated to Admin callers (or `SECRET_KEY`,
/// which is effective Admin); the active credential authorizes the call.
/// Returns the new user id. The new account is not logged in locally —
/// the admin stays signed in.
///
/// `tier` is the permission level to assign the new account.
#[tauri::command]
pub async fn auth_register(
    state: State<'_, AppStateHandle>,
    username: String,
    password: String,
    tier: PermissionTier,
) -> AppResult<String> {
    with_manager(&state, |m| async move {
        m.register(&username, &password, tier).await
    })
    .await
}

/// Change (or admin-reset) a user's password. The active credential
/// authorizes the call. `old_password` is empty for admin/secret-key
/// resets; required + verified server-side for non-admin self-changes.
/// `target_user_id` is the user whose password changes (self-change uses
/// the session's own id). The current session stays valid.
#[tauri::command]
pub async fn auth_change_password(
    state: State<'_, AppStateHandle>,
    target_user_id: String,
    old_password: String,
    new_password: String,
) -> AppResult<()> {
    with_manager(&state, |m| async move {
        m.change_password(&target_user_id, &old_password, &new_password)
            .await
    })
    .await
}

/// List every registered user (admin-gated server-side). Returns
/// `[{id, username, level}]` — no password hashes. Used by the client
/// to populate the admin password-reset dropdown.
#[tauri::command]
pub async fn auth_list_users(
    state: State<'_, AppStateHandle>,
) -> AppResult<Vec<crate::transport::UserEntry>> {
    with_manager(&state, |m| async move { m.list_users().await }).await
}

/// Delete a user account (admin-gated server-side). The active credential
/// authorizes the call. `target_user_id` is the UUID of the user to delete.
/// Removes the user row + cascades to sessions, follows, audit entries.
/// The deleted user's downloaded content remains in the local cache.
#[tauri::command]
pub async fn auth_delete_user(
    state: State<'_, AppStateHandle>,
    target_user_id: String,
) -> AppResult<()> {
    with_manager(
        &state,
        |m| async move { m.delete_user(&target_user_id).await },
    )
    .await
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
