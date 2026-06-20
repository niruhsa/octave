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
