//! Playlist-recommendation cache + warm-on-change (Phase 3 of PLAYLISTS_OPTS).
//!
//! Recomputing a playlist's recommendations on every open is the "recompute
//! every open" cost that makes an uncached playlist slow. This layer makes a
//! *warm* pool instant:
//!
//!   * [`RecommendationCache`] — an in-memory, TTL + size-bounded map from a
//!     **content signature** (a hash of the playlist's ordered track-id *set*,
//!     so a membership change busts it but a no-op reorder does not) to the
//!     precomputed rec track-ids. The read path in
//!     [`RecommendationService::recommend_for_playlist`](super::recommendation::RecommendationService::recommend_for_playlist)
//!     checks it first (µs on hit) and single-flights the miss so a request
//!     that races a background warm awaits one shared computation.
//!   * [`DebouncedWarmer`] — hooks the playlist membership mutations
//!     ([`PlaylistService`](super::playlist::PlaylistService) add / insert /
//!     remove) to fire a **debounced**, fire-and-forget recompute so the pool
//!     is already warm by the time the user opens the playlist.
//!
//! The cache key is the content signature only (not the playlist id) because the
//! rec read API is passed the playlist's current track ids, not its id — two
//! playlists with the same track set share the (identical) recs, and the warm
//! path derives the same signature from the mutated playlist's tracks.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::db::repo::PlaylistRepo;

use super::recommendation::RecommendationService;

/// How long a cached pool stays fresh. Long enough that reopening a playlist is
/// instant, short enough that newly-added library tracks eventually surface.
pub const REC_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60); // 12 h
/// Max distinct playlist signatures held before the oldest is evicted.
pub const REC_CACHE_MAX: usize = 1024;
/// Debounce window: coalesce a burst of edits (e.g. adding 20 tracks) into one
/// recompute fired shortly after the last change.
pub const REC_WARM_DEBOUNCE: Duration = Duration::from_secs(3);

struct Entry {
    ids: Vec<Uuid>,
    stored_at: Instant,
}

/// In-memory recommendation-pool cache (see the module docs).
pub struct RecommendationCache {
    ttl: Duration,
    max_entries: usize,
    entries: RwLock<HashMap<String, Entry>>,
    /// Per-key async locks for single-flight: concurrent misses on the same key
    /// serialize so only one computes while the rest await + reuse the result.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl RecommendationCache {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries: max_entries.max(1),
            entries: RwLock::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// The cached rec track-ids for `key`, or `None` when absent or expired.
    pub fn get(&self, key: &str) -> Option<Vec<Uuid>> {
        let entries = self.entries.read().unwrap();
        let entry = entries.get(key)?;
        if entry.stored_at.elapsed() > self.ttl {
            return None;
        }
        Some(entry.ids.clone())
    }

    /// Store (or refresh) the pool for `key`, evicting the oldest entry when at
    /// capacity (simple LRU-by-age).
    pub fn put(&self, key: String, ids: Vec<Uuid>) {
        let mut entries = self.entries.write().unwrap();
        if entries.len() >= self.max_entries && !entries.contains_key(&key) {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.stored_at)
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            Entry {
                ids,
                stored_at: Instant::now(),
            },
        );
    }

    /// The single-flight lock for `key` (shared across concurrent callers).
    pub fn flight_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

/// Schedules background recomputes of a playlist's recommendation pool. Injected
/// into [`PlaylistService`](super::playlist::PlaylistService); membership
/// mutations call [`schedule`](PlaylistRecWarmer::schedule) after committing.
pub trait PlaylistRecWarmer: Send + Sync {
    /// Fire-and-forget: schedule a debounced recompute for `playlist_id`. Never
    /// blocks the caller and never surfaces errors (warming is best-effort).
    fn schedule(&self, playlist_id: Uuid);
}

/// A [`PlaylistRecWarmer`] that debounces per playlist and recomputes off the
/// request path via `tokio::spawn`.
pub struct DebouncedWarmer {
    discover: Arc<RecommendationService>,
    playlists: Arc<dyn PlaylistRepo>,
    window: Duration,
    /// Playlists with a warm already scheduled (coalesces bursts).
    pending: Arc<Mutex<HashSet<Uuid>>>,
}

impl DebouncedWarmer {
    pub fn new(
        discover: Arc<RecommendationService>,
        playlists: Arc<dyn PlaylistRepo>,
        window: Duration,
    ) -> Self {
        Self {
            discover,
            playlists,
            window,
            pending: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl PlaylistRecWarmer for DebouncedWarmer {
    fn schedule(&self, playlist_id: Uuid) {
        // Coalesce: if a warm is already pending for this playlist, the existing
        // task will pick up the latest tracks when it fires — nothing to do.
        {
            let mut pending = self.pending.lock().unwrap();
            if !pending.insert(playlist_id) {
                return;
            }
        }
        let discover = self.discover.clone();
        let playlists = self.playlists.clone();
        let pending = self.pending.clone();
        let window = self.window;
        tokio::spawn(async move {
            tokio::time::sleep(window).await;
            // Clear the pending flag *before* recomputing so an edit arriving
            // during the recompute schedules a fresh follow-up pass.
            pending.lock().unwrap().remove(&playlist_id);
            match playlists.list_tracks(playlist_id).await {
                Ok(rows) => {
                    let ids: Vec<Uuid> = rows.into_iter().map(|r| r.track_id).collect();
                    if let Err(e) = discover.warm_playlist(&ids).await {
                        tracing::debug!(error = %e, %playlist_id, "playlist rec warm failed");
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, %playlist_id, "playlist rec warm: list_tracks failed");
                }
            }
        });
    }
}
