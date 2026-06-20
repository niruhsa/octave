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
//! - Each path is debounced via an in-flight `HashSet`; a 500 ms settle
//!   delay lets large writes finish before `lofty` probes them.

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
                if tx.blocking_send(path).is_err() {
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
        while let Some(path) = rx.recv().await {
            if !should_process(&path) {
                continue;
            }
            // Debounce: skip if already in flight.
            {
                let mut pending = debounce.lock().await;
                if !pending.insert(path.clone()) {
                    continue;
                }
            }
            let ingest = ingest.clone();
            let debounce = debounce.clone();
            tokio::spawn(async move {
                tokio::time::sleep(SETTLE).await;
                handle_path(&ingest, &path).await;
                debounce.lock().await.remove(&path);
            });
        }
        info!("ingest watcher channel closed; task exiting");
    });

    Ok(watcher)
}

/// Walk `root` once and feed every audio file through the ingest pipeline.
async fn scan_existing(ingest: &IngestService, root: &Path) {
    let mut count = 0u64;
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        if !should_process(&path) {
            continue;
        }
        handle_path(ingest, &path).await;
        count += 1;
    }
    if count > 0 {
        info!(scanned = count, root = %root.display(), "ingest pre-scan complete");
    }
}

async fn handle_path(ingest: &IngestService, path: &Path) {
    if !path.exists() {
        debug!(path = %path.display(), "watch: file disappeared, skipping");
        return;
    }
    // Source remains untouched — copy-only ingest.
    match ingest
        .organize_and_index(&Identity::SecretKey, path)
        .await
    {
        Ok(result) if result.already_indexed => {
            debug!(
                path = %path.display(),
                dest = %result.dest.display(),
                "watch: already indexed, no-op"
            );
        }
        Ok(result) => {
            info!(
                track_id = %result.track_id,
                src = %path.display(),
                dest = %result.dest.display(),
                "watch: ingested"
            );
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "watch: ingest failed");
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
    if path
        .components()
        .any(|c| {
            c.as_os_str()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
        })
    {
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
