//! Playlist view types — what the frontend renders.
//!
//! `MergedPlaylist` is the list-row shape (no entries; the detail view
//! fetches them). `MergedPlaylistEntry` pairs a 1-based `position` with the
//! same `MergedTrack` the library + player use, so a playlist row drops
//! straight into the player queue. `PlaylistDetailView` wraps the playlist
//! row + its ordered entries with a `source` tag like `LibraryView`.

use serde::{Deserialize, Serialize};

use crate::library::{LibrarySource, MergedTrack};

/// One playlist in the user's list. `entry_count` is the cached track count
/// (offline-cache principle: only downloaded entries are counted when the
/// source is `Cache`; the server's true count surfaces when `source` is
/// `Server`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedPlaylist {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    /// True when the id is a client-minted `local:` placeholder whose
    /// `playlist.create` op is still queued. The UI can badge these
    /// "unsynced" and disable server-only affordances.
    pub local: bool,
}

/// One ordered entry inside a playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedPlaylistEntry {
    /// 1-based contiguous position.
    pub position: i64,
    /// ISO-8601 added-at. Best-effort — the cache re-stamps rows on
    /// reorder/replace, so this is approximate for offline edits until the
    /// next sync pulls server truth.
    pub added_at: String,
    /// The track row, enriched from cache (downloaded entries) or a bounded
    /// server fetch (online, uncached entries). Offline + uncached entries
    /// carry a stub `MergedTrack` with empty fields + `downloaded=false`;
    /// the UI marks those "stream-only / not available offline".
    pub track: MergedTrack,
}

/// Wrapped detail payload so callers can branch on source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistDetailView {
    pub source: LibrarySource,
    pub playlist: MergedPlaylist,
    pub entries: Vec<MergedPlaylistEntry>,
}

impl MergedPlaylist {
    pub fn from_transport(p: crate::transport::Playlist) -> Self {
        let local = crate::sync::ops::is_local_id(&p.id);
        Self {
            id: p.id,
            owner_id: p.owner_id,
            name: p.name,
            local,
        }
    }

    pub fn from_cache(c: crate::cache::model::Playlist) -> Self {
        let local = crate::sync::ops::is_local_id(&c.id);
        Self {
            id: c.id,
            owner_id: c.owner_id,
            name: c.name,
            local,
        }
    }
}
