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
    pub updated_at: String,
}

/// One album whose metadata is cached locally.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Album {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i64>,
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
