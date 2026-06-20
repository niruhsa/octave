//! Client-side auth: secure credential storage + the active session state
//! used to attach a `Credential` to outbound calls.
//!
//! Storage strategy:
//!   * Desktop (macOS, Windows, Linux) — OS keychain via the `keyring`
//!     crate (Keychain / Credential Manager / libsecret).
//!   * Android — a file in app-private storage. Android's app sandbox +
//!     full-disk encryption protect the file at rest; we deliberately
//!     avoid the Android Keystore JNI dance for Phase 2 and revisit in a
//!     later hardening pass.
//!
//! State lifecycle:
//!   1. App boots. `AuthManager::restore_from_store` reads the keychain
//!      and re-hydrates the in-memory state if creds exist.
//!   2. User logs in / sets a `SECRET_KEY`. `AuthManager` writes the new
//!      cred to the store and updates in-memory state.
//!   3. User logs out. We call the server (best effort), wipe the store,
//!      clear in-memory state.

pub mod manager;
pub mod store;

pub use manager::{AuthManager, AuthSession};
pub use store::{StoredCredential, StoredCredentialKind};
