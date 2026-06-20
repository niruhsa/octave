//! Central error type used across the server.
//!
//! Each transport (gRPC / REST) maps `AppError` into its own status representation
//! at the edge; business logic always returns `Result<T>`.

use thiserror::Error;

/// Crate-wide `Result` alias.
pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    /// Configuration / environment problem.
    #[error("configuration error: {0}")]
    Config(String),

    /// Authentication failed (bad/missing credentials).
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),

    /// Authenticated but lacks permission.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Entity not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Bad request payload / validation failure.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Anything else not yet specialised.
    #[error("internal error: {0}")]
    Internal(String),
}
