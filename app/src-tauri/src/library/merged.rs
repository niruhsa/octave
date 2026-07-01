//! Merged view types — what the frontend renders. Combines the server's
//! catalog row with the local "is this downloaded?" flag.
//!
//! These are deliberately distinct from `transport::Artist` etc. so the UI
//! never has to ask "where did this come from?" — every list item already
//! carries its offline-availability bit, regardless of source.
//!
//! Offline-only items still produce a row; their `downloaded` is `true`
//! by definition (they wouldn't be in the cache otherwise).
//!
//! `local_cover_path` / `local_file_path` are populated **only** for
//! downloaded items — the server's `cover_path` / `file_path` stay
//! server-relative and aren't directly usable by the frontend renderer.

use serde::{Deserialize, Serialize};

use crate::cache::model as cache_model;
use crate::transport::{AliasInfo, Album, Artist, Track};

/// Default album classification when a source (older server / offline cache)
/// doesn't carry one.
fn default_album_type() -> String {
    "album".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedArtist {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    /// Server-side artist image path when set (presence drives whether the UI
    /// attempts to render the image). `None` for cache-sourced rows — the
    /// offline cache doesn't store artist images.
    pub image_path: Option<String>,
    /// Every known spelling (e.g. Korean + English), preserved across merges.
    /// Populated on single-entity reads only; empty for list/search/cache rows.
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
    /// Sum of the on-disk bytes of every track by this artist (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
    /// Has at least one track by this artist been downloaded locally? The
    /// service decides how to determine that — see `service.rs`. For
    /// offline-source results it's always `true`.
    pub downloaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedAlbum {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i64>,
    /// Classification: `album` | `ep` | `single`.
    #[serde(default = "default_album_type")]
    pub album_type: String,
    /// Server-side cover (might be `None`).
    pub cover_path: Option<String>,
    /// Local on-disk cover (from `album_art` table) when present.
    pub local_cover_path: Option<String>,
    /// Every known title spelling. See `MergedArtist::aliases`.
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
    /// Sum of the on-disk bytes of every track on this album (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
    pub downloaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedTrack {
    pub id: String,
    pub album_id: String,
    pub artist_id: String,
    pub title: String,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i64>,
    /// Server's path (used for streaming). Always present.
    pub file_path: String,
    pub file_size: Option<i64>,
    /// Audio-quality detail probed server-side. `None` when unknown.
    #[serde(default)]
    pub sample_rate_hz: Option<i64>,
    #[serde(default)]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub channels: Option<i64>,
    /// Local file when downloaded; `None` for stream-only items.
    pub local_file_path: Option<String>,
    /// `true` when this track is a single release within its album.
    #[serde(default)]
    pub is_single_release: bool,
    /// Every known title spelling (populated on single-track reads only).
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
    pub downloaded: bool,
}

// --- builders from server / cache rows ------------------------------------

impl MergedArtist {
    pub fn from_server(a: Artist, downloaded: bool) -> Self {
        Self {
            id: a.id,
            name: a.name,
            sort_name: a.sort_name,
            image_path: a.image_path,
            aliases: a.aliases,
            storage_bytes: a.storage_bytes,
            downloaded,
        }
    }

    pub fn from_cache(a: cache_model::Artist) -> Self {
        Self {
            id: a.id,
            name: a.name,
            sort_name: a.sort_name,
            // The offline cache doesn't store artist images.
            image_path: None,
            aliases: Vec::new(),
            storage_bytes: a.storage_bytes,
            downloaded: true,
        }
    }
}

impl MergedAlbum {
    pub fn from_server(a: Album, local_cover_path: Option<String>, downloaded: bool) -> Self {
        Self {
            id: a.id,
            artist_id: a.artist_id,
            title: a.title,
            release_year: a.release_year,
            album_type: a.album_type,
            cover_path: a.cover_path,
            local_cover_path,
            aliases: a.aliases,
            storage_bytes: a.storage_bytes,
            downloaded,
        }
    }

    pub fn from_cache(a: cache_model::Album, art: Option<cache_model::AlbumArt>) -> Self {
        Self {
            id: a.id,
            artist_id: a.artist_id,
            title: a.title,
            release_year: a.release_year,
            // The offline cache doesn't store the album type.
            album_type: default_album_type(),
            cover_path: None,
            local_cover_path: art.map(|x| x.local_cover_path),
            aliases: Vec::new(),
            storage_bytes: a.storage_bytes,
            downloaded: true,
        }
    }
}

impl MergedTrack {
    pub fn from_server(t: Track, local_file_path: Option<String>) -> Self {
        let downloaded = local_file_path.is_some();
        Self {
            id: t.id,
            album_id: t.album_id,
            artist_id: t.artist_id,
            title: t.title,
            track_no: t.track_no,
            disc_no: t.disc_no,
            duration_ms: t.duration_ms,
            codec: t.codec,
            bitrate_kbps: t.bitrate_kbps,
            file_path: t.file_path,
            file_size: t.file_size,
            sample_rate_hz: t.sample_rate_hz,
            bit_depth: t.bit_depth,
            channels: t.channels,
            local_file_path,
            is_single_release: t.is_single_release,
            aliases: t.aliases,
            downloaded,
        }
    }

    pub fn from_cache(t: cache_model::Track) -> Self {
        // Offline path: the server's `file_path` isn't known to us. Re-use
        // the local path so the UI has *something* to display in fields
        // that expect a non-empty server path. The downloaded flag is true.
        Self {
            id: t.id,
            album_id: t.album_id,
            artist_id: t.artist_id,
            title: t.title,
            track_no: t.track_no,
            disc_no: t.disc_no,
            duration_ms: t.duration_ms,
            codec: t.codec,
            bitrate_kbps: t.bitrate_kbps,
            file_path: t.local_file_path.clone(),
            file_size: t.file_size,
            sample_rate_hz: t.sample_rate_hz,
            bit_depth: t.bit_depth,
            channels: t.channels,
            local_file_path: Some(t.local_file_path),
            // The offline cache doesn't track the single-release flag.
            is_single_release: false,
            // The offline cache doesn't store title aliases.
            aliases: Vec::new(),
            downloaded: true,
        }
    }
}
