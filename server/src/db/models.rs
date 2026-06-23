//! Domain data types persisted by the repository layer.
//!
//! Kept deliberately plain so they can be reused unchanged on the client's
//! SQLite cache. JSON payloads are stored as `String` (TEXT) for portability.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Permission tier
// ---------------------------------------------------------------------------

/// User permission tier. `Admin > Manager > User` (each inherits the level
/// below). Stored as TEXT in the DB so the same value survives the trip into
/// the client's SQLite cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PermissionLevel {
    Admin,
    Manager,
    User,
}

impl PermissionLevel {
    /// Inheritance check: does `self` satisfy the requirement of `required`?
    pub fn satisfies(self, required: PermissionLevel) -> bool {
        self.rank() >= required.rank()
    }

    fn rank(self) -> u8 {
        match self {
            PermissionLevel::User => 1,
            PermissionLevel::Manager => 2,
            PermissionLevel::Admin => 3,
        }
    }
}

// ---------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub permission_level: PermissionLevel,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Artist {
    pub id: Uuid,
    pub name: String,
    pub sort_name: Option<String>,
    /// Path to a manager-uploaded artist image under `ARTWORK_PATH`, or
    /// `None` when no image has been set. Served via `GET /artists/:id/image`.
    pub image_path: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Album {
    pub id: Uuid,
    pub artist_id: Uuid,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    pub album_id: Uuid,
    pub artist_id: Uuid,
    pub title: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i32>,
    pub file_path: String,
    pub file_size: Option<i64>,
    /// JSON-as-TEXT; validated at the service layer.
    pub metadata_json: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Playlist {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub playlist_id: Uuid,
    pub track_id: Uuid,
    pub position: i32,
    pub added_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Follow {
    pub user_id: Uuid,
    pub artist_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: Uuid,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------
// Create payloads (insert-shape DTOs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct NewUser {
    pub username: String,
    pub password_hash: String,
    pub permission_level: PermissionLevel,
}

#[derive(Debug, Clone)]
pub struct NewArtist {
    pub name: String,
    pub sort_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAlbum {
    pub artist_id: Uuid,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewTrack {
    pub album_id: Uuid,
    pub artist_id: Uuid,
    pub title: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i32>,
    pub file_path: String,
    pub file_size: Option<i64>,
    pub metadata_json: String,
}

#[derive(Debug, Clone)]
pub struct NewPlaylist {
    pub owner_id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct NewAuditEntry {
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub token: String,
    pub user_id: Uuid,
    pub expires_at: OffsetDateTime,
}

// ---------------------------------------------------------------------------
// Uploads (v2): DB-backed, session-oriented, per-chunk-verified uploads.
// ---------------------------------------------------------------------------

/// Lifecycle of an upload session. Stored as TEXT (portable to SQLite).
///
/// `initialized` → `uploading` → `completed`, or `cancelled` from either
/// active state. Per-chunk hash failures don't advance state (the chunk POST
/// just fails); whole-file/ingest errors are captured in the completion report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UploadState {
    Initialized,
    Uploading,
    Completed,
    Cancelled,
}

impl UploadState {
    /// The two states in which an upload is still in flight (counts toward the
    /// one-active-upload-per-user limit; cancellable).
    pub fn is_active(self) -> bool {
        matches!(self, UploadState::Initialized | UploadState::Uploading)
    }

    /// Parse a wire/query string into a state, for `?state=` filters.
    pub fn parse(s: &str) -> Option<UploadState> {
        match s.to_ascii_lowercase().as_str() {
            "initialized" => Some(UploadState::Initialized),
            "uploading" => Some(UploadState::Uploading),
            "completed" => Some(UploadState::Completed),
            "cancelled" => Some(UploadState::Cancelled),
            _ => None,
        }
    }
}

/// Per-file progress within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UploadFileState {
    Pending,
    Uploading,
    Complete,
    Failed,
}

/// An upload session row (the report's top level).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Upload {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub state: UploadState,
    pub total_files: i32,
    pub total_bytes: i64,
    pub report_json: Option<String>,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One file within an upload session.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct UploadFile {
    pub id: Uuid,
    pub upload_id: Uuid,
    pub file_index: i32,
    pub filename: String,
    pub file_hash: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_chunks: i32,
    pub received_chunks: i32,
    pub state: UploadFileState,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
}

/// One chunk of one file. Presence (`received`) + `hash` give resumability and
/// integrity. The bytes live on disk; this row is the metadata/state authority.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct UploadChunk {
    pub upload_file_id: Uuid,
    pub chunk_index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub hash: String,
    pub received: bool,
    pub received_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewUpload {
    pub user_id: Option<Uuid>,
    pub total_files: i32,
    pub total_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct NewUploadFile {
    pub upload_id: Uuid,
    pub file_index: i32,
    pub filename: String,
    pub file_hash: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_chunks: i32,
}

#[derive(Debug, Clone)]
pub struct NewUploadChunk {
    pub upload_file_id: Uuid,
    pub chunk_index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub hash: String,
}

/// Filter for `UploadRepo::list_uploads`.
#[derive(Debug, Clone, Default)]
pub struct UploadFilter {
    /// Restrict to a single owner. `None` = no owner filter (admin: all users).
    pub user_id: Option<Uuid>,
    /// Restrict to a single state.
    pub state: Option<UploadState>,
}
