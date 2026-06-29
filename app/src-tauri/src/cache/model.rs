//! Cache row types. Field shapes mirror the SQLite schema in
//! `migrations/0001_init.sql` and the server's PostgreSQL model. IDs are
//! server-issued — never generated client-side.

use serde::{Deserialize, Serialize};

/// One artist whose metadata is cached locally.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    /// Sum of the on-disk bytes of every track owned by this artist (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
    pub updated_at: String,
}

/// One album whose metadata is cached locally.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Album {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i64>,
    /// Sum of the on-disk bytes of every track on this album (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
    pub updated_at: String,
}

/// A downloaded album cover.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AlbumArt {
    pub album_id: String,
    pub local_cover_path: String,
    pub fetched_at: String,
}

/// One fully-downloaded track. Presence of a row here is the source of truth
/// for "this track is available offline".
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Track {
    pub id: String,
    pub album_id: String,
    pub artist_id: String,
    pub title: String,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i64>,
    pub file_size: Option<i64>,
    /// Audio-quality detail probed server-side. `None` when unknown.
    #[serde(default)]
    pub sample_rate_hz: Option<i64>,
    #[serde(default)]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub channels: Option<i64>,
    pub local_file_path: String,
    pub metadata_json: String,
    pub downloaded_at: String,
    pub updated_at: String,
}

/// A playlist row mirrored from the server.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Playlist {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub updated_at: String,
}

/// A single entry inside a playlist. `track_id` may reference a track that
/// is *not* cached locally — the UI marks those as stream-only when offline.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PlaylistTrack {
    pub playlist_id: String,
    pub track_id: String,
    pub position: i64,
    pub added_at: String,
}

/// One queued play awaiting flush to the server (Phase 11). A send-only
/// outbox: the server owns the authoritative history. `completed` is `0`/`1`;
/// `played_at` is when the play happened (the insert time).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PendingPlay {
    pub id: String,
    pub track_id: String,
    pub ms_played: i64,
    pub completed: i64,
    pub played_at: String,
}

/// One queued offline edit awaiting replay against the server. `op_type`
/// selects the payload shape; `payload_json` is decoded by the sync engine.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PendingOp {
    pub id: i64,
    pub op_type: String,
    pub payload_json: String,
    pub created_at: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

/// Reconciliation bookkeeping for one cached entity.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SyncState {
    pub entity_type: String,
    pub entity_id: String,
    pub server_version: Option<String>,
    pub server_etag: Option<String>,
    pub last_synced_at: String,
}

/// A subscribed podcast show, cached so the subscription list renders offline.
/// Mirrors the server `podcasts` row minus the refresh bookkeeping. `subscribed`
/// is `0`/`1`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Podcast {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub language: Option<String>,
    pub categories: String,
    pub subscribed: i64,
    /// Sum of the on-disk bytes of every downloaded episode of this show (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
    pub updated_at: String,
}

/// One fully-downloaded podcast episode. Presence of a row here (with a
/// `local_file_path`) is the source of truth for "available offline", exactly
/// like `Track`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PodcastEpisode {
    pub id: String,
    pub podcast_id: String,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub episode_no: Option<i64>,
    pub season_no: Option<i64>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i64>,
    pub file_size: Option<i64>,
    pub local_file_path: Option<String>,
    pub image_path: Option<String>,
    pub published_at: Option<String>,
    pub metadata_json: String,
    pub downloaded_at: Option<String>,
    pub updated_at: String,
}

/// The single user's playback progress on one episode. Mirrors the server's
/// per-user `EpisodeProgress`, minus the user_id (the client is one account).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PodcastEpisodeProgress {
    pub episode_id: String,
    pub position_ms: i64,
    /// 1 = played to (near) the end. Stored as INTEGER in SQLite.
    pub completed: i64,
    pub updated_at: String,
}
