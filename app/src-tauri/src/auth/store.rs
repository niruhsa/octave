//! Secure credential storage.
//!
//! Exposes a single `SecureStore` trait with two implementations:
//!   * `KeyringStore` — OS keychain on desktop platforms.
//!   * `FileStore`    — app-private file fallback (Android, or when the
//!     keychain is unavailable, e.g. headless Linux without a secret
//!     service).
//!
//! Stored value is a JSON-serialised [`StoredCredential`] so we can evolve
//! the shape (e.g. add `expires_at` checks) without changing the storage
//! contract.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::transport::PermissionTier;

/// Service name used in the OS keychain. Matches Tauri's app identifier so
/// uninstalling the app cleanly removes the entry on platforms that scope
/// keychain entries to the bundle id (macOS).
pub const KEYRING_SERVICE: &str = "dev.niruhsa.music.app";
/// Single keychain entry name — we store one credential at a time.
pub const KEYRING_USER: &str = "default";

/// On-disk filename used by `FileStore`.
pub const CRED_FILENAME: &str = "credential.json";

/// Tagged discriminator so we don't lose track of which kind of credential
/// is in the store (frontend cares: bearer can expire; secret-key can't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredCredentialKind {
    SecretKey,
    Bearer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    pub kind: StoredCredentialKind,
    pub secret: String,
    /// Server URLs — persisted so the app can auto-reconnect on restart
    /// without the user re-entering the server address. `#[serde(default)]`
    /// for backward compat with entries saved before this field was added.
    #[serde(default)]
    pub rest_url: Option<String>,
    #[serde(default)]
    pub grpc_url: Option<String>,
    /// Whether `grpc_url` was an explicit user override (vs derived from the
    /// REST URL). Persisted so a custom gRPC endpoint survives restarts; a
    /// derived URL is re-derived so it keeps tracking the REST URL. `None`
    /// on legacy entries — treated as "derived" (re-derive on restore).
    #[serde(default)]
    pub grpc_explicit: Option<bool>,
    /// Optional cached identity from the last successful `WhoAmI` /
    /// `Login`. Treated as advisory — the server is authority on tier.
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub tier: Option<PermissionTier>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[async_trait]
pub trait SecureStore: Send + Sync {
    async fn load(&self) -> AppResult<Option<StoredCredential>>;
    async fn save(&self, cred: &StoredCredential) -> AppResult<()>;
    async fn clear(&self) -> AppResult<()>;
}

// ---------------------------------------------------------------------------
// FileStore (used on Android and as a desktop fallback)
// ---------------------------------------------------------------------------

/// File-backed credential store at a caller-chosen path. The directory must
/// already exist (we lazily `create_dir_all` on first save).
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl SecureStore for FileStore {
    async fn load(&self) -> AppResult<Option<StoredCredential>> {
        let path = self.path.clone();
        let bytes = tokio::task::spawn_blocking(move || match std::fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        })
        .await
        .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        .map_err(|e| AppError::SecureStorage(format!("read: {e}")))?;

        match bytes {
            None => Ok(None),
            Some(b) => serde_json::from_slice::<StoredCredential>(&b)
                .map(Some)
                .map_err(|e| AppError::SecureStorage(format!("decode: {e}"))),
        }
    }

    async fn save(&self, cred: &StoredCredential) -> AppResult<()> {
        let path = self.path.clone();
        let bytes = serde_json::to_vec(cred)
            .map_err(|e| AppError::SecureStorage(format!("encode: {e}")))?;
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Best-effort owner-only perms on Unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&path)?;
                f.write_all(&bytes)?;
                f.sync_all()?;
                return Ok(());
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&path, &bytes)?;
                Ok(())
            }
        })
        .await
        .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        .map_err(|e| AppError::SecureStorage(format!("write: {e}")))?;
        Ok(())
    }

    async fn clear(&self) -> AppResult<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        })
        .await
        .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        .map_err(|e| AppError::SecureStorage(format!("remove: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// KeyringStore (desktop)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
pub use desktop_keyring::KeyringStore;

#[cfg(not(target_os = "android"))]
mod desktop_keyring {
    use super::{
        AppError, AppResult, SecureStore, StoredCredential, KEYRING_SERVICE, KEYRING_USER,
    };
    use async_trait::async_trait;

    /// OS keychain-backed store. All calls are blocking — wrapped in
    /// `spawn_blocking` so they don't stall the async runtime.
    pub struct KeyringStore;

    impl KeyringStore {
        pub fn new() -> Self {
            Self
        }

        fn entry() -> AppResult<keyring::Entry> {
            keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
                .map_err(|e| AppError::SecureStorage(format!("keyring open: {e}")))
        }
    }

    #[async_trait]
    impl SecureStore for KeyringStore {
        async fn load(&self) -> AppResult<Option<StoredCredential>> {
            tokio::task::spawn_blocking(|| {
                let entry = Self::entry()?;
                match entry.get_password() {
                    Ok(s) => serde_json::from_str::<StoredCredential>(&s)
                        .map(Some)
                        .map_err(|e| AppError::SecureStorage(format!("decode: {e}"))),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(AppError::SecureStorage(format!("keyring read: {e}"))),
                }
            })
            .await
            .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        }

        async fn save(&self, cred: &StoredCredential) -> AppResult<()> {
            let serialised = serde_json::to_string(cred)
                .map_err(|e| AppError::SecureStorage(format!("encode: {e}")))?;
            tokio::task::spawn_blocking(move || {
                Self::entry()?
                    .set_password(&serialised)
                    .map_err(|e| AppError::SecureStorage(format!("keyring write: {e}")))
            })
            .await
            .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        }

        async fn clear(&self) -> AppResult<()> {
            tokio::task::spawn_blocking(|| match Self::entry()?.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(AppError::SecureStorage(format!("keyring delete: {e}"))),
            })
            .await
            .map_err(|e| AppError::SecureStorage(format!("join: {e}")))?
        }
    }
}
