//! App-wide error type. Serializable so commands can return it across the
//! Tauri bridge without losing structure.

use serde::Serialize;

/// Result alias used by `#[tauri::command]` handlers and internal subsystems.
pub type AppResult<T> = std::result::Result<T, AppError>;

/// Errors surfaced to the frontend. Variants are intentionally coarse; add
/// granularity as subsystems land.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum AppError {
    /// SQLite / migration / pool problems.
    #[error("database error: {0}")]
    Database(String),

    /// Anything related to talking to the server: refused connection,
    /// timeout, codec error, non-auth HTTP error. NOT for auth rejections.
    #[error("transport error: {0}")]
    Transport(String),

    /// Server returned 401 / `Unauthenticated` — credentials missing,
    /// invalid, or expired. UI should kick the user back to login.
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),

    /// Server returned 403 / `PermissionDenied` — authenticated but tier
    /// is too low. UI should hide the affordance.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// Auth state is missing / corrupt locally (no creds saved, secure
    /// store unreadable). UI prompts for login.
    #[error("auth not configured: {0}")]
    AuthNotConfigured(String),

    /// Secure credential store failure (keychain unavailable, encrypted
    /// file unreadable, etc.).
    #[error("secure storage error: {0}")]
    SecureStorage(String),

    /// Anything not yet mapped to a richer variant.
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Internal(format!("{err:#}"))
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Database(err.to_string())
    }
}

impl From<sqlx::migrate::MigrateError> for AppError {
    fn from(err: sqlx::migrate::MigrateError) -> Self {
        AppError::Database(format!("migration: {err}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Internal(format!("io: {err}"))
    }
}
