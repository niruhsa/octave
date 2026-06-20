//! In-memory active-session state, persisted through a `SecureStore`.
//!
//! Holds the current `Credential` (if any), the most recent `WhoAmI`
//! snapshot, and a debounced online/offline flag. Commands read this via
//! Tauri's managed state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::store::{SecureStore, StoredCredential, StoredCredentialKind};
use crate::error::{AppError, AppResult};
use crate::transport::{Credential, PermissionTier, ServerClient, ServerConfig};

/// A snapshot of the active session safe to hand to the frontend. Mirrors
/// `StoredCredential` minus the secret material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub kind: StoredCredentialKind,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub tier: PermissionTier,
    pub expires_at: Option<String>,
}

pub struct AuthManager {
    server: Arc<ServerClient>,
    store: Arc<dyn SecureStore>,
    state: RwLock<Option<StoredCredential>>,
    online: RwLock<bool>,
}

impl AuthManager {
    pub fn new(server: Arc<ServerClient>, store: Arc<dyn SecureStore>) -> Self {
        Self {
            server,
            store,
            state: RwLock::new(None),
            online: RwLock::new(false),
        }
    }

    /// Read the persisted credential (if any) into memory. Called once at
    /// startup. Errors during read are logged and treated as "no session" —
    /// a corrupt keychain entry shouldn't brick the app.
    pub async fn restore_from_store(&self) {
        match self.store.load().await {
            Ok(Some(cred)) => {
                tracing::info!(kind = ?cred.kind, "restored auth credential from secure store");
                *self.state.write().await = Some(cred);
            }
            Ok(None) => tracing::debug!("no stored credential"),
            Err(e) => tracing::warn!(err = %e, "failed to restore credential; continuing anonymous"),
        }
    }

    pub async fn current(&self) -> Option<AuthSession> {
        self.state.read().await.as_ref().map(|c| AuthSession {
            kind: c.kind,
            user_id: c.user_id.clone(),
            username: c.username.clone(),
            tier: c.tier.unwrap_or(PermissionTier::User),
            expires_at: c.expires_at.clone(),
        })
    }

    /// Return the active `Credential` for an outbound call, or
    /// `AuthNotConfigured` if nobody has logged in / set a key.
    pub async fn credential(&self) -> AppResult<Credential> {
        let guard = self.state.read().await;
        let cred = guard
            .as_ref()
            .ok_or_else(|| AppError::AuthNotConfigured("no active session".into()))?;
        Ok(match cred.kind {
            StoredCredentialKind::SecretKey => Credential::SecretKey(cred.secret.clone()),
            StoredCredentialKind::Bearer => Credential::Bearer(cred.secret.clone()),
        })
    }

    pub async fn is_online(&self) -> bool {
        *self.online.read().await
    }

    /// Run a `/health` ping and update the cached online flag.
    pub async fn refresh_online(&self) -> bool {
        let ok = self.server.health().await.unwrap_or(false);
        *self.online.write().await = ok;
        ok
    }

    /// Username/password login. Persists the resulting bearer token.
    pub async fn login(&self, username: &str, password: &str) -> AppResult<AuthSession> {
        let outcome = self.server.login(username, password).await?;
        let cred = StoredCredential {
            kind: StoredCredentialKind::Bearer,
            secret: outcome.token.clone(),
            user_id: Some(outcome.user_id.clone()),
            username: Some(username.to_string()),
            tier: Some(outcome.tier),
            expires_at: Some(outcome.expires_at.clone()),
        };
        self.store.save(&cred).await?;
        *self.state.write().await = Some(cred);
        *self.online.write().await = true;
        Ok(AuthSession {
            kind: StoredCredentialKind::Bearer,
            user_id: Some(outcome.user_id),
            username: Some(username.to_string()),
            tier: outcome.tier,
            expires_at: Some(outcome.expires_at),
        })
    }

    /// Install a pre-shared `SECRET_KEY` as the active credential. We
    /// verify it via `WhoAmI` before persisting so the user sees an
    /// immediate failure for typos instead of a silent broken state.
    pub async fn set_secret_key(&self, key: &str) -> AppResult<AuthSession> {
        let probe = Credential::SecretKey(key.to_string());
        let who = self.server.whoami(&probe).await?;
        let cred = StoredCredential {
            kind: StoredCredentialKind::SecretKey,
            secret: key.to_string(),
            user_id: None,
            username: None,
            tier: Some(who.tier),
            expires_at: None,
        };
        self.store.save(&cred).await?;
        *self.state.write().await = Some(cred);
        *self.online.write().await = true;
        Ok(AuthSession {
            kind: StoredCredentialKind::SecretKey,
            user_id: None,
            username: None,
            tier: who.tier,
            expires_at: None,
        })
    }

    /// Resolve the current credential against the server. Updates the
    /// cached tier so the UI reflects server-side changes (e.g. an admin
    /// downgraded the user) on next refresh.
    pub async fn whoami(&self) -> AppResult<AuthSession> {
        let cred = self.credential().await?;
        let who = self.server.whoami(&cred).await?;
        let mut guard = self.state.write().await;
        if let Some(c) = guard.as_mut() {
            c.tier = Some(who.tier);
            if !who.user_id.is_empty() {
                c.user_id = Some(who.user_id.clone());
            }
            if !who.username.is_empty() {
                c.username = Some(who.username.clone());
            }
            self.store.save(c).await?;
        }
        Ok(AuthSession {
            kind: guard.as_ref().map(|c| c.kind).unwrap_or(StoredCredentialKind::Bearer),
            user_id: if who.user_id.is_empty() { None } else { Some(who.user_id) },
            username: if who.username.is_empty() { None } else { Some(who.username) },
            tier: who.tier,
            expires_at: guard.as_ref().and_then(|c| c.expires_at.clone()),
        })
    }

    /// Log out: best-effort server revocation, then wipe local state.
    pub async fn logout(&self) -> AppResult<()> {
        if let Ok(cred) = self.credential().await {
            if let Err(e) = self.server.logout(&cred).await {
                tracing::warn!(err = %e, "server logout failed; clearing local state anyway");
            }
        }
        self.store.clear().await?;
        *self.state.write().await = None;
        Ok(())
    }

    pub fn server(&self) -> &ServerClient {
        &self.server
    }

    pub fn server_config(&self) -> ServerConfig {
        self.server.config().clone()
    }
}
