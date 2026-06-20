//! Environment-driven configuration.
//!
//! On startup we locate the project's `.env` file and load it into the
//! process environment. The directory containing that `.env` is then used
//! as the **config anchor**: any relative filesystem path (e.g.
//! `LIBRARY_PATH=./library`) is resolved against it, so the meaning of a
//! relative path doesn't depend on what directory the server was launched
//! from.
//!
//! Search order for the `.env` file:
//!   1. `ENV_FILE` env var (explicit override; absolute or relative).
//!   2. Walk up from the current working directory, looking for `.env`.
//!   3. `CARGO_MANIFEST_DIR/.env` at compile time (dev / `cargo run`).
//!   4. None — config is taken straight from the process environment and
//!      relative paths anchor to the current working directory.
//!
//! Defaults are chosen to match the Docker Compose deployment described
//! in `PLAN.md` Phase 13.

use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::{AppError, Result};

/// Top-level runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the gRPC server binds to (primary transport).
    pub grpc_addr: SocketAddr,
    /// Address the REST fallback binds to.
    pub rest_addr: SocketAddr,
    /// Pre-shared secret key for the `SECRET_KEY` auth mechanism.
    pub secret_key: String,
    /// Whether the optional admin UI should be started.
    pub enable_admin_ui: bool,
    /// PostgreSQL connection string (used from Phase 1 onward).
    pub database_url: Option<String>,
    /// Filesystem path where the organised library lives.
    /// Absolute. Resolved from `LIBRARY_PATH` relative to the config anchor.
    pub library_path: Option<PathBuf>,
    /// Filesystem path of the ingest folder (copy-only).
    /// Absolute. Resolved from `INGEST_PATH` relative to the config anchor.
    pub ingest_path: Option<PathBuf>,
    /// Whether metadata edits are written back to the file's audio tags via
    /// `lofty`. Off by default (DB stays authoritative; files untouched).
    pub write_tags: bool,
    /// Whether album artwork is fetched automatically from an external
    /// source (Cover Art Archive). Off by default.
    pub fetch_artwork: bool,
    /// Directory where fetched album artwork is cached. Absolute. Resolved
    /// from `ARTWORK_PATH` relative to the config anchor; defaults to
    /// `<library_path>/.artwork` when unset and a library path exists.
    pub artwork_path: Option<PathBuf>,
    /// Directory that relative paths anchor to. Either the dir containing
    /// the loaded `.env` file or the current working directory.
    pub config_anchor: PathBuf,
}

impl Config {
    /// Load configuration from environment variables, seeding from `.env`
    /// if one is found (see module docs for the search order).
    pub fn from_env() -> Result<Self> {
        let anchor = load_dotenv_and_anchor();

        let grpc_addr = parse_addr("GRPC_ADDR", "0.0.0.0:50051")?;
        let rest_addr = parse_addr("REST_ADDR", "0.0.0.0:8080")?;

        let secret_key = env::var("SECRET_KEY")
            .map_err(|_| AppError::Config("SECRET_KEY is required".into()))?;

        let enable_admin_ui = env::var("ENABLE_ADMIN_UI")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        let database_url = env::var("DATABASE_URL").ok();
        let library_path = env::var("LIBRARY_PATH")
            .ok()
            .map(|p| resolve_path(&anchor, &p));
        let ingest_path = env::var("INGEST_PATH")
            .ok()
            .map(|p| resolve_path(&anchor, &p));
        let write_tags = env_flag("WRITE_TAGS");
        let fetch_artwork = env_flag("FETCH_ARTWORK");
        let artwork_path = env::var("ARTWORK_PATH")
            .ok()
            .map(|p| resolve_path(&anchor, &p))
            .or_else(|| library_path.as_ref().map(|l| l.join(".artwork")));

        if let Some(p) = &library_path {
            debug!(resolved = %p.display(), "LIBRARY_PATH resolved");
        }
        if let Some(p) = &ingest_path {
            debug!(resolved = %p.display(), "INGEST_PATH resolved");
        }

        Ok(Self {
            grpc_addr,
            rest_addr,
            secret_key,
            enable_admin_ui,
            database_url,
            library_path,
            ingest_path,
            write_tags,
            fetch_artwork,
            artwork_path,
            config_anchor: anchor,
        })
    }
}

/// Locate a `.env` file, load it, and return the directory it lives in
/// (the **config anchor**). Falls back to the current working directory
/// when no `.env` is found.
fn load_dotenv_and_anchor() -> PathBuf {
    if let Some(path) = locate_env_file() {
        // `dotenvy::from_path` doesn't override pre-set env vars, matching
        // the default `dotenv()` behaviour.
        match dotenvy::from_path(&path) {
            Ok(()) => info!(env_file = %path.display(), "loaded .env"),
            Err(e) => warn!(env_file = %path.display(), error = %e, "failed to load .env"),
        }
        if let Some(parent) = path.parent() {
            return parent.to_path_buf();
        }
    } else {
        debug!("no .env file found; using process environment as-is");
    }

    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Find the `.env` file we should load. See the module docs for ordering.
fn locate_env_file() -> Option<PathBuf> {
    // 1. Explicit override.
    if let Ok(raw) = env::var("ENV_FILE") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            return Some(p);
        }
        warn!(env_file = %p.display(), "ENV_FILE set but file does not exist");
    }

    // 2. Walk upward from CWD.
    if let Ok(cwd) = env::current_dir() {
        let mut cursor: Option<&Path> = Some(&cwd);
        while let Some(dir) = cursor {
            let candidate = dir.join(".env");
            if candidate.is_file() {
                return Some(candidate);
            }
            cursor = dir.parent();
        }
    }

    // 3. Compile-time crate root (dev / cargo-run).
    let manifest_env = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env");
    if manifest_env.is_file() {
        return Some(manifest_env);
    }

    None
}

/// Resolve `raw` against `anchor` when relative. Absolute paths pass
/// through unchanged. Trailing slashes and `~` are not expanded here —
/// users wanting `$HOME` expansion should use absolute paths in `.env`.
fn resolve_path(anchor: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let p = PathBuf::from(trimmed);
    if p.is_absolute() { p } else { anchor.join(p) }
}

/// Parse a boolean-ish env var. Absent / unrecognised => `false`.
fn env_flag(var: &str) -> bool {
    env::var(var)
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn parse_addr(var: &str, default: &str) -> Result<SocketAddr> {
    let raw = env::var(var).unwrap_or_else(|_| default.to_string());
    raw.parse::<SocketAddr>()
        .map_err(|e| AppError::Config(format!("invalid {var}={raw}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_keeps_absolute() {
        let anchor = PathBuf::from("/srv/music-server");
        assert_eq!(
            resolve_path(&anchor, "/var/music/library"),
            PathBuf::from("/var/music/library")
        );
    }

    #[test]
    fn resolve_path_joins_relative_to_anchor() {
        let anchor = PathBuf::from("/srv/music-server");
        assert_eq!(
            resolve_path(&anchor, "./library"),
            PathBuf::from("/srv/music-server/./library")
        );
        assert_eq!(
            resolve_path(&anchor, "library"),
            PathBuf::from("/srv/music-server/library")
        );
        assert_eq!(
            resolve_path(&anchor, "../shared/library"),
            PathBuf::from("/srv/music-server/../shared/library")
        );
    }

    #[test]
    fn resolve_path_trims_whitespace() {
        let anchor = PathBuf::from("/srv");
        assert_eq!(
            resolve_path(&anchor, "  ingest  "),
            PathBuf::from("/srv/ingest")
        );
    }
}
