//! Library storage accounting.
//!
//! Maintains two things, both stored in the DB for fast querying:
//!   * Per-entity `storage_bytes` rollups (artist/album/podcast) — the SUM of
//!     the on-disk bytes of the files each owns.
//!   * A singleton `library_storage` breakdown (music / podcast / artwork /
//!     other) powering the homepage widget.
//!
//! Two recompute paths:
//!   * [`recompute_aggregates`](StorageService::recompute_aggregates) — pure SQL
//!     (sums + counts + per-entity rollups). Cheap; run after every upload and
//!     at the end of every scan.
//!   * [`recompute_disk`](StorageService::recompute_disk) — a filesystem walk
//!     for the artwork/other byte split. Heavier; run on scans + the 24h job.
//!
//! The 24h background job ([`spawn_refresh_job`](StorageService::spawn_refresh_job))
//! does a *light* library refresh — index new files, drop rows for files that
//! vanished, then a full recompute — so the stats track reality without a
//! per-file re-tag.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};
use walkdir::WalkDir;

use crate::auth::Identity;
use crate::db::models::LibraryStorage;
use crate::db::repo::StorageRepo;
use crate::error::{AppError, Result};
use crate::services::scan::ScanService;
use crate::services::tag;

/// Floor for the refresh-job cadence regardless of config (mirrors the podcast
/// poller's floor) — a misconfigured tiny interval shouldn't hammer the disk.
const MIN_REFRESH_SECS: u64 = 300;

#[derive(Clone)]
pub struct StorageService {
    storage: Arc<dyn StorageRepo>,
    /// Artwork cache dir (`ARTWORK_PATH`). Counted as `artwork_bytes`.
    artwork_path: Option<PathBuf>,
    /// Organised music library root (`LIBRARY_PATH`).
    library_path: Option<PathBuf>,
    /// Podcast storage root (`PODCAST_PATH`), if the subsystem is enabled.
    podcast_path: Option<PathBuf>,
}

impl StorageService {
    pub fn new(
        storage: Arc<dyn StorageRepo>,
        library_path: Option<PathBuf>,
        artwork_path: Option<PathBuf>,
        podcast_path: Option<PathBuf>,
    ) -> Self {
        Self {
            storage,
            artwork_path,
            library_path,
            podcast_path,
        }
    }

    /// Cheap recompute: per-entity rollups + the SQL-derived global fields
    /// (music/podcast bytes, counts). No filesystem access — safe to call after
    /// every upload.
    pub async fn recompute_aggregates(&self) -> Result<()> {
        self.storage.recompute_entity_storage().await?;
        let agg = self.storage.aggregates().await?;
        self.storage.set_library_aggregates(agg).await?;
        Ok(())
    }

    /// Heavier recompute: walk the artwork dir and the library/podcast roots to
    /// split disk usage into `artwork_bytes` (the cache dir) and `other_bytes`
    /// (non-audio files elsewhere — embedded covers, metadata sidecars, etc.).
    /// Audio files are excluded here because they're already counted as music /
    /// podcast bytes from the DB.
    pub async fn recompute_disk(&self) -> Result<()> {
        let artwork = self.artwork_path.clone();
        // Library + podcast roots, de-nested so an overlapping pair (the default
        // `PODCAST_PATH = <LIBRARY_PATH>/Podcasts`) isn't walked twice.
        let roots = dedup_roots(
            [self.library_path.clone(), self.podcast_path.clone()]
                .into_iter()
                .flatten()
                .collect(),
        );
        let (artwork_bytes, other_bytes) = tokio::task::spawn_blocking(move || {
            let artwork_bytes = artwork.as_deref().map(dir_size).unwrap_or(0);
            let other_bytes = other_files_size(&roots, artwork.as_deref());
            (artwork_bytes, other_bytes)
        })
        .await
        .map_err(|e| AppError::Internal(format!("storage walk join: {e}")))?;
        self.storage
            .set_library_disk(artwork_bytes, other_bytes)
            .await?;
        Ok(())
    }

    /// Both recompute passes (used by scans + the 24h job + startup).
    pub async fn recompute_all(&self) -> Result<()> {
        self.recompute_aggregates().await?;
        self.recompute_disk().await?;
        Ok(())
    }

    /// Read the singleton breakdown row — the fast path for the widget.
    pub async fn get_stats(&self) -> Result<LibraryStorage> {
        self.storage.get_library_storage().await
    }

    /// Spawn the background storage work: always a one-shot startup recompute
    /// (so the widget has data immediately, fast — no full disk re-scan), then,
    /// when `interval_secs > 0`, a periodic light refresh (incremental scan +
    /// prune + full recompute). `caller` must be a Manager+ system identity
    /// (scan/prune are privileged).
    pub fn spawn_refresh_job(&self, scan: ScanService, caller: Identity, interval_secs: u64) {
        let this = self.clone();
        tokio::spawn(async move {
            // Startup pass: recompute only.
            if let Err(e) = this.recompute_all().await {
                warn!(error = %e, "storage refresh: startup recompute failed");
            }
            if interval_secs == 0 {
                return; // periodic job disabled
            }
            let period = Duration::from_secs(interval_secs.max(MIN_REFRESH_SECS));
            let mut tick = tokio::time::interval(period);
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                this.run_light_refresh(&scan, &caller).await;
            }
        });
    }

    /// One light-refresh cycle: index new files, prune rows whose file vanished,
    /// then recompute every stat. Each step logs and continues on error.
    pub async fn run_light_refresh(&self, scan: &ScanService, caller: &Identity) {
        match scan.scan(caller, None).await {
            Ok(rep) => info!(
                added = rep.tracks_added,
                skipped = rep.tracks_skipped,
                errors = rep.errors,
                "storage refresh: incremental scan complete"
            ),
            Err(e) => warn!(error = %e, "storage refresh: scan failed"),
        }
        match scan.prune_missing(caller).await {
            Ok(n) if n > 0 => info!(removed = n, "storage refresh: pruned missing files"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "storage refresh: prune failed"),
        }
        if let Err(e) = self.recompute_all().await {
            warn!(error = %e, "storage refresh: recompute failed");
        }
    }
}

/// Total bytes of every regular file under `dir` (recursive). Best-effort:
/// unreadable entries are skipped.
fn dir_size(dir: &Path) -> i64 {
    if !dir.is_dir() {
        return 0;
    }
    let mut total: i64 = 0;
    for entry in WalkDir::new(dir).follow_links(false).into_iter().flatten() {
        if entry.file_type().is_file() {
            if let Ok(md) = entry.metadata() {
                total += md.len() as i64;
            }
        }
    }
    total
}

/// Bytes of every **non-audio** file under `roots`, excluding anything inside
/// `artwork` (counted separately) — the "misc/other" bucket. Audio files are
/// skipped because they're accounted for as music/podcast bytes from the DB.
fn other_files_size(roots: &[PathBuf], artwork: Option<&Path>) -> i64 {
    let mut total: i64 = 0;
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if let Some(art) = artwork {
                if path.starts_with(art) {
                    continue; // counted in artwork_bytes
                }
            }
            if tag::is_audio_file(path) {
                continue; // counted in music/podcast bytes
            }
            if let Ok(md) = entry.metadata() {
                total += md.len() as i64;
            }
        }
    }
    total
}

/// Drop any root that is a descendant of another root in the set, so an
/// overlapping pair isn't walked twice. Order-independent.
fn dedup_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for r in roots {
        // Skip if `r` is inside an already-kept root.
        if kept.iter().any(|k| r.starts_with(k)) {
            continue;
        }
        // Drop any kept root that is inside `r` (r is the broader one).
        kept.retain(|k| !k.starts_with(&r));
        kept.push(r);
    }
    kept
}
