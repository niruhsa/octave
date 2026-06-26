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
    /// Optional gRPC TLS. `Some` when `GRPC_TLS` is enabled; the server then
    /// presents this identity to clients (which must connect over `https`).
    pub grpc_tls: Option<GrpcTlsConfig>,
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
    /// Max dimension (px, longest side) that cached cover/artist images are
    /// downscaled to when optimized. `IMAGE_MAX_DIM`, default 800.
    pub image_max_dim: u32,
    /// JPEG quality (1–100) for optimized images. `IMAGE_QUALITY`, default 82.
    pub image_quality: u8,
    /// How often the background optimize-all pass runs, in seconds.
    /// `IMAGE_OPTIMIZE_INTERVAL_SECS`, default 21600 (6h); 0 disables it.
    pub image_optimize_interval_secs: u64,
    /// Language whose spelling is shown as the canonical artist/album name when
    /// merged duplicates carry multiple spellings. `PRIMARY_LANGUAGE`
    /// (normalized to a label like `"English"`); defaults to `"English"`.
    /// A per-user setting later; an env var for now.
    pub primary_language: String,
    /// Optional Firebase Cloud Messaging push. `Some` when `FCM_ENABLED` is on;
    /// the new-release fan-out then also pushes to followers' registered
    /// devices. Off by default (the client polls instead).
    pub fcm: Option<FcmConfig>,
    /// Optional podcast subsystem. `Some` when `PODCAST_PATH` is set, or when
    /// `LIBRARY_PATH` is set (defaults to `<LIBRARY_PATH>/Podcasts`). `None`
    /// disables the whole feature.
    pub podcast: Option<PodcastConfig>,
    /// Directory that relative paths anchor to. Either the dir containing
    /// the loaded `.env` file or the current working directory.
    pub config_anchor: PathBuf,
}

/// Podcast subsystem config. The feature is enabled whenever a `path` resolves
/// (explicit `PODCAST_PATH`, else `<LIBRARY_PATH>/Podcasts`).
#[derive(Debug, Clone)]
pub struct PodcastConfig {
    /// Where episode audio + show art are stored. Absolute (anchor-resolved).
    pub path: PathBuf,
    /// Feed refresh poller cadence in seconds. 0 disables the poller.
    pub refresh_interval_secs: u64,
    /// Default newest-N auto-download for a freshly-subscribed show.
    pub auto_download_default: i32,
    /// Optional PodcastIndex API credentials (richer search). `None` → iTunes
    /// only. Both key + secret required together.
    pub podcastindex: Option<PodcastIndexCreds>,
}

/// PodcastIndex API credentials. Only used when both are present.
#[derive(Debug, Clone)]
pub struct PodcastIndexCreds {
    pub api_key: String,
    pub api_secret: String,
}

/// Firebase Cloud Messaging config (Phase 10 — real-time push). The credentials
/// file is a Google **service-account JSON key** (used to mint an OAuth2 token
/// for the FCM HTTP v1 API); only its path is held, never the key bytes.
#[derive(Debug, Clone)]
pub struct FcmConfig {
    /// Firebase project id (the `messages:send` URL embeds it).
    pub project_id: String,
    /// Service-account JSON key path. Absolute (resolved against the anchor).
    pub credentials_path: PathBuf,
}

/// PEM file paths for gRPC TLS. Paths only — never the key bytes, which must
/// not end up in `Debug` output or logs.
#[derive(Debug, Clone)]
pub struct GrpcTlsConfig {
    /// PEM-encoded certificate (chain). Absolute (resolved against the anchor).
    pub cert_path: PathBuf,
    /// PEM-encoded private key. Absolute (resolved against the anchor).
    pub key_path: PathBuf,
}

impl Config {
    /// Load configuration from environment variables, seeding from `.env`
    /// if one is found (see module docs for the search order).
    pub fn from_env() -> Result<Self> {
        let anchor = load_dotenv_and_anchor();

        let grpc_addr = parse_addr("GRPC_ADDR", "0.0.0.0:50051")?;
        let grpc_tls = load_grpc_tls(&anchor)?;
        let fcm = load_fcm(&anchor)?;
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

        let podcast = load_podcast(&anchor, library_path.as_ref())?;

        // Image optimization knobs (sensible defaults; all overridable).
        let image_max_dim = env_u64("IMAGE_MAX_DIM", 800).clamp(64, 8192) as u32;
        let image_quality = env_u64("IMAGE_QUALITY", 82).clamp(1, 100) as u8;
        let image_optimize_interval_secs = env_u64("IMAGE_OPTIMIZE_INTERVAL_SECS", 21_600);

        // Primary display language: normalize a set value to a canonical label
        // (so `en`/`english`/`English` all work); default English.
        let primary_language = env::var("PRIMARY_LANGUAGE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|v| crate::services::tag::normalize_language(&v))
            .unwrap_or_else(|| "English".to_string());

        if let Some(p) = &library_path {
            debug!(resolved = %p.display(), "LIBRARY_PATH resolved");
        }
        if let Some(p) = &ingest_path {
            debug!(resolved = %p.display(), "INGEST_PATH resolved");
        }

        Ok(Self {
            grpc_addr,
            grpc_tls,
            rest_addr,
            secret_key,
            enable_admin_ui,
            database_url,
            library_path,
            ingest_path,
            write_tags,
            fetch_artwork,
            artwork_path,
            image_max_dim,
            image_quality,
            image_optimize_interval_secs,
            primary_language,
            fcm,
            podcast,
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

/// Parse a `u64` env var, falling back to `default` when absent or unparseable.
fn env_u64(var: &str, default: u64) -> u64 {
    env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Optional gRPC TLS, enabled by the `GRPC_TLS` flag. When on, `GRPC_TLS_CERT`
/// and `GRPC_TLS_KEY` (PEM file paths, resolved against the config anchor) are
/// both required — a missing one is a hard config error so TLS never silently
/// falls back to plaintext.
fn load_grpc_tls(anchor: &Path) -> Result<Option<GrpcTlsConfig>> {
    if !env_flag("GRPC_TLS") {
        return Ok(None);
    }
    let cert = env::var("GRPC_TLS_CERT").map_err(|_| {
        AppError::Config("GRPC_TLS is enabled but GRPC_TLS_CERT (cert PEM path) is not set".into())
    })?;
    let key = env::var("GRPC_TLS_KEY").map_err(|_| {
        AppError::Config("GRPC_TLS is enabled but GRPC_TLS_KEY (key PEM path) is not set".into())
    })?;
    Ok(Some(GrpcTlsConfig {
        cert_path: resolve_path(anchor, &cert),
        key_path: resolve_path(anchor, &key),
    }))
}

/// Optional FCM push, enabled by the `FCM_ENABLED` flag. When on,
/// `FCM_PROJECT_ID` and `FCM_CREDENTIALS` (service-account JSON path, resolved
/// against the config anchor) are both required — a missing one is a hard
/// config error so push is never silently half-configured.
fn load_fcm(anchor: &Path) -> Result<Option<FcmConfig>> {
    if !env_flag("FCM_ENABLED") {
        return Ok(None);
    }
    let project_id = env::var("FCM_PROJECT_ID").map_err(|_| {
        AppError::Config("FCM_ENABLED is on but FCM_PROJECT_ID is not set".into())
    })?;
    let credentials = env::var("FCM_CREDENTIALS").map_err(|_| {
        AppError::Config(
            "FCM_ENABLED is on but FCM_CREDENTIALS (service-account JSON path) is not set".into(),
        )
    })?;
    Ok(Some(FcmConfig {
        project_id,
        credentials_path: resolve_path(anchor, &credentials),
    }))
}

/// Optional podcast subsystem. Enabled whenever a storage path resolves:
/// explicit `PODCAST_PATH`, else `<LIBRARY_PATH>/Podcasts`. `None` (no
/// `PODCAST_PATH` and no `LIBRARY_PATH`) disables the feature cleanly.
fn load_podcast(anchor: &Path, library_path: Option<&PathBuf>) -> Result<Option<PodcastConfig>> {
    let path = match env::var("PODCAST_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        Some(p) => Some(resolve_path(anchor, &p)),
        None => library_path.map(|l| l.join("Podcasts")),
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let refresh_interval_secs = env_u64("PODCAST_REFRESH_INTERVAL_SECS", 1800);
    let auto_download_default =
        env_u64("PODCAST_AUTO_DOWNLOAD_DEFAULT", 0).min(i32::MAX as u64) as i32;
    let podcastindex = load_podcastindex()?;
    Ok(Some(PodcastConfig {
        path,
        refresh_interval_secs,
        auto_download_default,
        podcastindex,
    }))
}

/// PodcastIndex creds — both `PODCASTINDEX_API_KEY` + `PODCASTINDEX_API_SECRET`
/// or neither (a half-config is a hard error, like `FCM_*` / `GRPC_TLS_*`).
fn load_podcastindex() -> Result<Option<PodcastIndexCreds>> {
    let key = env::var("PODCASTINDEX_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let secret = env::var("PODCASTINDEX_API_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty());
    match (key, secret) {
        (Some(api_key), Some(api_secret)) => Ok(Some(PodcastIndexCreds { api_key, api_secret })),
        (None, None) => Ok(None),
        _ => Err(AppError::Config(
            "PODCASTINDEX_API_KEY and PODCASTINDEX_API_SECRET must both be set (or neither)".into(),
        )),
    }
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
