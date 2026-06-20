//! Postgres connection pool + migrations.

use sqlx::postgres::{PgPool, PgPoolOptions};
use tracing::info;

use crate::error::{AppError, Result};

/// Build a Postgres connection pool from `DATABASE_URL`.
pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(database_url)
        .await
        .map_err(|e| AppError::Internal(format!("db connect failed: {e}")))?;
    info!("postgres connection pool established");
    Ok(pool)
}

/// Apply all pending migrations from `server/migrations/`.
///
/// Migrations are embedded into the binary at compile time so the runtime
/// image (and the Docker container) does not need to ship the SQL files.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| AppError::Internal(format!("migrations failed: {e}")))?;
    info!("database migrations applied");
    Ok(())
}
