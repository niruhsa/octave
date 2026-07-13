//! Local SQLite offline cache — the partial mirror of the server DB.
//!
//! Stores ONLY items the user has downloaded for offline use (track metadata
//! with `local_file_path`, album metadata, artists, cover paths, playlists,
//! sync bookkeeping). Never the full catalog. The server is authority when
//! online; this DB is the fallback when it isn't.
//!
//! Schema lives in [`crate::db::MIGRATIONS`] (embedded `migrations/`). Every
//! row's primary key equals the server's primary key — Phase 5 (Sync Engine)
//! relies on that contract.

use std::path::{Path, PathBuf};

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::AppResult;

/// Embedded migrations — included in the binary so a fresh install on any
/// platform can bring up an empty DB without shipping SQL files alongside.
pub static MIGRATIONS: Migrator = sqlx::migrate!("./migrations");

/// Default filename for the cache DB inside the OS app-data dir.
pub const DB_FILENAME: &str = "cache.sqlite";

/// Open (or create) the cache DB at `db_path`, run pending migrations, and
/// hand back a connection pool.
///
/// `db_path` must be inside the platform's app-private storage:
///   * Desktop  — `app_data_dir()` (e.g. `~/Library/Application Support/...`).
///   * Android  — internal app storage (scoped storage, app-private).
///   * iOS      — app sandbox `Documents`/`Application Support`.
/// Choosing the path is the caller's job (see `lib.rs`).
pub async fn open(db_path: &Path) -> AppResult<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        // WAL gives us concurrent reads while a writer is active — useful
        // for sync + UI queries overlapping.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    MIGRATIONS.run(&pool).await?;

    tracing::info!(path = %db_path.display(), "offline cache db ready");
    Ok(pool)
}

/// Resolve the default cache DB path under an app-data dir.
pub fn default_db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(DB_FILENAME)
}

#[cfg(test)]
pub async fn open_in_memory() -> AppResult<SqlitePool> {
    // A single connection is required for SQLite's per-connection `:memory:`
    // database. This helper keeps focused repository tests fast and isolated.
    let opts = SqliteConnectOptions::new()
        .filename(":memory:")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    MIGRATIONS.run(&pool).await?;
    Ok(pool)
}
