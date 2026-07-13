//! Background ingest-folder watcher.
//!
//! Watches the configured `INGEST_PATH` directory using `notify`.  When an
//! audio file appears it is **copied** into the library layout and indexed.
//! The source file is never moved or deleted.
//!
//! Behaviour notes:
//! - On startup the watcher does a one-shot **pre-scan** of `INGEST_PATH`,
//!   so files already present (or dropped while the server was offline)
//!   get picked up without waiting for a new event.
//! - The recursive watch is registered against `INGEST_PATH` itself.
//!   `notify` (FSEvents on macOS, inotify on Linux) walks newly-created
//!   subdirectories automatically, so dropping
//!   `Lang/Artist/Album/Track.flac` into the root works.
//! - We match a deliberately broad set of `EventKind`s. macOS FSEvents
//!   often coalesces a create-then-write into a single `Any` / `Create(Any)`
//!   event, and Linux can fire `Modify(Data)` without a preceding `Create`
//!   when files arrive via `mv` from the same filesystem.
//! - `.uploading` staging files (REST upload temp path) are skipped.
//! - Ingest is **folder-grouped**: a file event enqueues the file's *parent
//!   directory*, and the whole directory is ingested as a single album (see
//!   [`IngestService::organize_dir`]). This is what stops a 5-file album from
//!   fragmenting into 5 single-track albums. Directories are debounced via an
//!   in-flight `HashSet`; a 500 ms settle delay lets large writes finish
//!   before `lofty` probes them, and because `organize_dir` is idempotent a
//!   later file landing in the same folder simply re-runs it and adds the
//!   stragglers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::auth::Identity;
use crate::error::Result;
use crate::services::ingest::IngestService;
use crate::services::tag;

/// Settle window between event arrival and `lofty` probe. Long enough for a
/// large FLAC drop to finish writing, short enough that interactive testing
/// doesn't feel sluggish.
const SETTLE: Duration = Duration::from_millis(500);

/// Start the background folder watcher.
///
/// Returns a [`RecommendedWatcher`] handle that stays alive as long as the
/// caller holds it.  When dropped, the OS watch is removed automatically.
pub fn start(ingest: IngestService) -> Result<RecommendedWatcher> {
    let root = match ingest.ingest_root.clone() {
        Some(r) => r,
        None => {
            info!("INGEST_PATH not set; ingest watcher disabled");
            return noop_watcher();
        }
    };

    // Create the ingest dir if missing — otherwise the watch registration
    // below fails and the user sees a silent no-op.
    if !root.exists() {
        if let Err(e) = std::fs::create_dir_all(&root) {
            warn!(path = %root.display(), error = %e, "failed to create ingest root; watcher disabled");
            return noop_watcher();
        }
        info!(path = %root.display(), "created missing INGEST_PATH");
    }
    if !root.is_dir() {
        warn!(path = %root.display(), "INGEST_PATH is not a directory; watcher disabled");
        return noop_watcher();
    }

    // One-shot pre-scan: catch files dropped while the server was offline.
    // Done on a blocking task so we don't stall startup if the folder is large.
    {
        let ingest = ingest.clone();
        let root = root.clone();
        tokio::spawn(async move {
            scan_existing(&ingest, &root).await;
        });
    }

    // The channel carries *directories* to ingest (one album per folder), not
    // individual files.
    let (tx, mut rx) = mpsc::channel::<PathBuf>(256);
    let debounce: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let event = match res {
                Ok(e) => e,
                Err(err) => {
                    warn!(error = %err, "watcher event error");
                    return;
                }
            };
            if !is_interesting(&event.kind) {
                return;
            }
            for path in event.paths {
                // Only audio files trigger ingest; enqueue their *parent
                // directory* so the whole folder is grouped into one album.
                if !should_process(&path) {
                    continue;
                }
                let Some(dir) = path.parent().map(Path::to_path_buf) else {
                    continue;
                };
                if tx.blocking_send(dir).is_err() {
                    return; // receiver dropped
                }
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| crate::error::AppError::Internal(format!("watcher create: {e}")))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| crate::error::AppError::Internal(format!("watcher watch: {e}")))?;

    info!(path = %root.display(), "ingest folder watcher started");

    tokio::spawn(async move {
        while let Some(dir) = rx.recv().await {
            // Debounce: skip if this directory is already in flight.
            {
                let mut pending = debounce.lock().await;
                if !pending.insert(dir.clone()) {
                    continue;
                }
            }
            let ingest = ingest.clone();
            let debounce = debounce.clone();
            tokio::spawn(async move {
                tokio::time::sleep(SETTLE).await;
                handle_dir(&ingest, &dir).await;
                debounce.lock().await.remove(&dir);
            });
        }
        info!("ingest watcher channel closed; task exiting");
    });

    Ok(watcher)
}

/// Walk `root` once and ingest every folder that holds audio files, grouping
/// each folder into a single album (the "dropped while offline" catch-up).
async fn scan_existing(ingest: &IngestService, root: &Path) {
    // Collect the distinct parent directories of every processable audio file,
    // preserving first-seen order for deterministic logging.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !should_process(&path) {
            continue;
        }
        if let Some(parent) = path.parent() {
            let parent = parent.to_path_buf();
            if !dirs.contains(&parent) {
                dirs.push(parent);
            }
        }
    }
    for dir in &dirs {
        handle_dir(ingest, dir).await;
    }
    if !dirs.is_empty() {
        info!(dirs = dirs.len(), root = %root.display(), "ingest pre-scan complete");
    }
}

/// Ingest a single directory as one album (copy-only; sources untouched).
async fn handle_dir(ingest: &IngestService, dir: &Path) {
    if !dir.is_dir() {
        debug!(dir = %dir.display(), "watch: directory disappeared, skipping");
        return;
    }
    match ingest.organize_dir(&Identity::SecretKey, dir).await {
        Ok(result) if result.ingested > 0 => {
            info!(
                dir = %dir.display(),
                ingested = result.ingested,
                already_indexed = result.already_indexed,
                errors = result.errors,
                "watch: album ingested"
            );
            // New tracks landed via the ingest folder — refresh the cheap
            // storage aggregates so the stats track them.
            ingest.scan.recompute_storage_aggregates().await;
        }
        Ok(result) => {
            debug!(
                dir = %dir.display(),
                already_indexed = result.already_indexed,
                errors = result.errors,
                "watch: nothing new in directory"
            );
        }
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "watch: dir ingest failed");
        }
    }
}

/// Filter that runs both for live events and for pre-scan entries.
fn should_process(path: &Path) -> bool {
    if !tag::is_audio_file(path) {
        return false;
    }
    if is_uploading(path) {
        return false;
    }
    // `.tmp` staging dir under INGEST_PATH/.tmp is ours; skip anything
    // whose path components include a leading-dot directory.
    if path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    }) {
        return false;
    }
    true
}

/// `true` for event kinds that may indicate a new audio file landed.
///
/// We accept any `Create` or `Modify` variant rather than the narrow
/// `Create(File)` / `Modify(Data)` pair because macOS FSEvents coalesces
/// many filesystem operations into less-specific kinds (e.g. `Any`), and
/// `mv` from the same filesystem on Linux frequently emits only
/// `Modify(Name)` or `Modify(Other)`.  `Access`/`Remove` are filtered out.
fn is_interesting(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_)
    )
}

fn is_uploading(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("uploading"))
        .unwrap_or(false)
}

/// A watcher that does nothing — used when `INGEST_PATH` is unset/invalid
/// so the caller still gets a `RecommendedWatcher` handle.
fn noop_watcher() -> Result<RecommendedWatcher> {
    let (tx, _rx) = mpsc::channel::<PathBuf>(1);
    RecommendedWatcher::new(
        move |_: notify::Result<Event>| {
            let _ = &tx;
        },
        notify::Config::default(),
    )
    .map_err(|e| crate::error::AppError::Internal(format!("watcher init: {e}")))
}
