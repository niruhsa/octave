//! Look-ahead prefetch of the next streamed track to a local temp file.
//!
//! ## Why
//!
//! The webview's `<audio>` element can't *start* a network media load while the
//! page is hidden (screen off) and nothing is currently playing — Chromium
//! suspends it. So at the end of a streamed track the next one never buffers and
//! playback just stops. A *downloaded* track resolves to a local file that loads
//! instantly, so it advances fine — which is exactly why only streaming stalls.
//!
//! The fix makes streamed tracks look local: while a track plays, the frontend
//! asks us (`player_prefetch`) to fetch the *next* track. We download it to a
//! temp file over the Rust HTTP client — which has no such restriction, the same
//! capability the download foreground service already relies on — and the
//! loopback server ([`super::server`]) serves that file directly. The webview
//! then only ever loads local files at a track boundary, which never get
//! suspended.
//!
//! The cache is transient (cleared each launch) and capped at a handful of
//! files; we only ever need the immediately-upcoming track, and each track start
//! prefetches the next, so the chain walks the queue on its own.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use axum::http::header;
use tauri::{Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::AppStateHandle;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::player::server::{auth_header_value, proxy_client};
use crate::transport::Credential;

/// Files to keep on disk at once. We only need the next track; the margin covers
/// the just-played + currently-serving + next without ever evicting a file
/// that's still being served (playback only moves forward, one prefetch ahead).
const MAX_ENTRIES: usize = 4;

/// Retries for a transient failure — a blip shouldn't permanently leave the next
/// track un-prefetched, which would re-introduce the end-of-track stall.
const MAX_ATTEMPTS: usize = 3;

struct Entry {
    path: PathBuf,
    complete: bool,
    /// Content-Type captured from the stream response. The temp file has no
    /// extension, so the loopback can't guess the MIME from the path — and the
    /// `<audio>` element may reject `application/octet-stream`.
    content_type: Option<String>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Insertion order, for evicting the oldest past the cap.
    order: VecDeque<String>,
}

/// Tauri-managed state: the transient prefetch directory + bookkeeping.
pub struct PrefetchCache {
    dir: PathBuf,
    inner: Mutex<Inner>,
}

impl PrefetchCache {
    /// Create the cache, clearing anything left by a previous run (the files are
    /// transient and a stale half-download must never be served).
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Path + Content-Type of a *complete* prefetched file for `track_id`, if
    /// present. The loopback `serve` handler calls this; an in-progress download
    /// returns `None` so a partial file is never served.
    pub fn ready(&self, track_id: &str) -> Option<(PathBuf, String)> {
        let inner = self.inner.lock().ok()?;
        inner.entries.get(track_id).filter(|e| e.complete).map(|e| {
            let ct = e
                .content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string());
            (e.path.clone(), ct)
        })
    }

    /// Claim a slot for `track_id`. Returns the target path, or `None` if it's
    /// already prefetching/done. Evicts the oldest entries past the cap.
    fn claim(&self, track_id: &str) -> Option<PathBuf> {
        let mut inner = self.inner.lock().ok()?;
        if inner.entries.contains_key(track_id) {
            return None;
        }
        let path = self.dir.join(file_stem(track_id));
        inner.entries.insert(
            track_id.to_string(),
            Entry {
                path: path.clone(),
                complete: false,
                content_type: None,
            },
        );
        inner.order.push_back(track_id.to_string());
        // The oldest entries are the furthest-back tracks, no longer being
        // served (playback moved on), so deleting their files is safe.
        while inner.order.len() > MAX_ENTRIES {
            if let Some(old) = inner.order.pop_front() {
                if let Some(e) = inner.entries.remove(&old) {
                    let _ = std::fs::remove_file(&e.path);
                }
            }
        }
        Some(path)
    }

    fn mark_complete(&self, track_id: &str, content_type: Option<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(e) = inner.entries.get_mut(track_id) {
                e.complete = true;
                e.content_type = content_type;
            }
        }
    }

    /// Drop a failed/aborted entry and remove its partial file.
    fn discard(&self, track_id: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(e) = inner.entries.remove(track_id) {
                let _ = std::fs::remove_file(&e.path);
            }
            inner.order.retain(|t| t != track_id);
        }
    }
}

/// Kick off a prefetch of `track_id` in the background (idempotent, non-blocking).
pub fn spawn(app: tauri::AppHandle, track_id: String) {
    tauri::async_runtime::spawn(async move {
        run(app, track_id).await;
    });
}

async fn run(app: tauri::AppHandle, track_id: String) {
    let Some(state) = app.try_state::<AppStateHandle>() else {
        return;
    };
    // Already downloaded? The loopback serves that permanent file — nothing to do.
    if matches!(repo::get_track(&state.pool, &track_id).await, Ok(Some(_))) {
        return;
    }
    let Some(cache) = app.try_state::<PrefetchCache>() else {
        return;
    };
    let Some(path) = cache.claim(&track_id) else {
        return; // already prefetching or done
    };

    // Resolve auth + the stream URL once.
    let auth = state.auth.read().await.clone();
    let Some(auth) = auth else {
        cache.discard(&track_id);
        return;
    };
    let cred = match auth.credential().await {
        Ok(c) => c,
        Err(_) => {
            cache.discard(&track_id);
            return;
        }
    };
    let url = format!("{}/tracks/{}/stream", auth.server_config().rest_root(), track_id);

    let mut last_err: Option<AppError> = None;
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        match download(&url, &cred, &path).await {
            Ok(content_type) => {
                cache.mark_complete(&track_id, content_type);
                tracing::debug!(track = %track_id, "prefetched next track");
                // Tell the playback deck its standby source is now a local
                // file. The deck must not preload a streamed track before
                // this: the loopback would proxy-stream it in parallel with
                // this download (see GAPLESS_CROSSFADE.md §1).
                let _ = app.emit("player-prefetch-ready", &track_id);
                return;
            }
            Err(e) => last_err = Some(e),
        }
    }
    tracing::warn!(track = %track_id, err = ?last_err, "prefetch failed; will stream on demand");
    cache.discard(&track_id);
}

/// Download the authed track stream straight to `path`, returning the response's
/// Content-Type. Mirrors the loopback proxy's request
/// (`super::server::proxy_stream`) but writes to disk.
async fn download(url: &str, cred: &Credential, path: &Path) -> AppResult<Option<String>> {
    let client = proxy_client()?;
    let resp = client
        .get(url)
        .header(header::AUTHORIZATION, auth_header_value(cred)?)
        .send()
        .await
        .map_err(|e| AppError::Transport(format!("prefetch send: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Transport(format!(
            "prefetch status {}",
            resp.status()
        )));
    }
    // Capture the MIME before consuming the body (the temp file has no extension).
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| AppError::Internal(format!("prefetch create {}: {e}", path.display())))?;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Transport(format!("prefetch body: {e}")))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::Internal(format!("prefetch write: {e}")))?;
    }
    file.flush()
        .await
        .map_err(|e| AppError::Internal(format!("prefetch flush: {e}")))?;
    Ok(content_type)
}

/// A filesystem-safe stem for a track id (ids are UUIDs; this just defends
/// against a stray separator).
fn file_stem(track_id: &str) -> String {
    track_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ready()` (what `player_prefetch_is_ready` exposes to the deck) must
    /// only surface *complete* files: never a fresh claim (in-progress
    /// download) and never a discarded one.
    #[test]
    fn ready_only_after_complete() {
        let dir = std::env::temp_dir().join(format!(
            "octave-prefetch-test-{}",
            std::process::id()
        ));
        let cache = PrefetchCache::new(dir.clone());

        assert!(cache.ready("t1").is_none(), "unknown id is not ready");

        let path = cache.claim("t1").expect("fresh claim yields a path");
        assert!(cache.claim("t1").is_none(), "double-claim is refused");
        assert!(cache.ready("t1").is_none(), "in-progress is not ready");

        cache.mark_complete("t1", Some("audio/flac".into()));
        let (p, ct) = cache.ready("t1").expect("complete file is ready");
        assert_eq!(p, path);
        assert_eq!(ct, "audio/flac");

        cache.discard("t1");
        assert!(cache.ready("t1").is_none(), "discarded is not ready");

        let _ = std::fs::remove_dir_all(dir);
    }
}
