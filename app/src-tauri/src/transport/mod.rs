//! Server transport layer.
//!
//! gRPC primary, REST fallback — same preference order as the server. Most
//! callers go through [`ServerClient`], which owns both an active gRPC
//! channel and a REST client and falls back automatically when the gRPC side
//! is unreachable.
//!
//! Auth credentials are attached at the call site via [`Credential`], so the
//! transport itself stays stateless: rotating the session token or
//! switching from `SecretKey` to `Bearer` is just a different argument on
//! the next call.
//!
//! Module layout:
//!   * [`config`] — `ServerConfig { base_url }`.
//!   * [`proto`]  — tonic-generated gRPC stubs from the server's `.proto`.
//!   * [`grpc`]   — `tonic` client wired to the auth service.
//!   * [`rest`]   — `reqwest` client mirroring the same operations.
//!   * [`client`] — `ServerClient` that orchestrates gRPC → REST fallback.

pub mod client;
pub mod config;
pub mod grpc;
pub mod proto;
pub mod rest;

pub use client::{LoginOutcome, ServerClient, TransportHealth, TransportUsed, WhoAmI};
pub use config::ServerConfig;

use serde::{Deserialize, Serialize};

/// Server's view of an artist. Distinct from `crate::cache::model::Artist`
/// because the cache row carries an `updated_at` only the cache writes,
/// and the server's REST/gRPC payloads don't include it on these reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
}

/// Server's view of an album. `cover_path` is server-relative — not yet
/// a local file; downloads land in Phase 6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i64>,
    pub cover_path: Option<String>,
}

/// Server's view of a track. `file_path` is the server-side path; the
/// client streams it via the streaming endpoint (Phase 4) when online.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub file_path: String,
    pub file_size: Option<i64>,
    pub metadata_json: String,
}

/// Server's view of a playlist (no timestamps in the wire DTO).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub owner_id: String,
    pub name: String,
}

/// One server-side playlist entry. `position` is 1-based + contiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub playlist_id: String,
    pub track_id: String,
    pub position: i64,
}

// ── Uploads (Phase 8) ─────────────────────────────────────────────────

/// Result of a single-file upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleUploadResult {
    pub track_id: String,
    pub path: String,
}

/// Result of an archive upload (zip/tarball).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveUploadResult {
    pub kind: String,
    pub ingested: u64,
    pub already_indexed: u64,
    pub non_audio_skipped: u64,
    pub errors: u64,
    pub track_ids: Vec<String>,
}

/// Tagged union — the server returns either a single-file or archive result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "variant", content = "data")]
pub enum UploadResult {
    #[serde(rename = "single")]
    Single(SingleUploadResult),
    #[serde(rename = "archive")]
    Archive(ArchiveUploadResult),
}

// ── Uploads v2: session-oriented, per-chunk-verified ───────────────────

/// One chunk's declared `[start, end)` range + expected content hash.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkInit {
    pub index: u32,
    pub start: u64,
    pub end: u64,
    /// SHA-256 (lowercase hex) of this chunk's bytes.
    pub hash: String,
}

/// One file in a session: where the reassembled bytes go, the whole-file hash,
/// and the chunk map.
#[derive(Debug, Clone, Serialize)]
pub struct UploadFileInit {
    pub filename: String,
    pub hash: String,
    pub total_size: u64,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub chunks: Vec<ChunkInit>,
}

/// `POST /uploads/init` body — declares a whole session (a list of files).
#[derive(Debug, Clone, Serialize)]
pub struct UploadInitRequest {
    pub files: Vec<UploadFileInit>,
}

/// Outcome of one `put_chunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkAck {
    pub file_index: u32,
    pub chunk_index: u32,
    pub received_chunks: u32,
    pub total_chunks: u32,
    pub file_complete: bool,
    pub upload_complete: bool,
    pub state: String,
}

/// One chunk's detail in a session report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadChunkView {
    pub index: u32,
    pub start: u64,
    pub end: u64,
    pub hash: String,
    pub received: bool,
}

/// One file's detail in a session report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFileView {
    pub file_index: u32,
    pub filename: String,
    pub file_hash: String,
    pub total_size: u64,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub received_chunks: u32,
    pub state: String,
    pub error: Option<String>,
    pub chunks: Vec<UploadChunkView>,
}

/// A full upload session report (`GET /uploads/:id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadView {
    pub id: String,
    pub user_id: Option<String>,
    pub state: String,
    pub total_files: u32,
    pub total_bytes: u64,
    pub bytes_received: u64,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
    pub report: Option<serde_json::Value>,
    pub files: Vec<UploadFileView>,
}

/// A lightweight session row (`GET /uploads` list item).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadSummary {
    pub id: String,
    pub user_id: Option<String>,
    pub state: String,
    pub total_files: u32,
    pub total_bytes: u64,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
}

/// A live progress event from the `uploads` channel (gRPC stream or WS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadEvent {
    pub kind: String,
    pub upload_id: String,
    pub owner_id: Option<String>,
    pub state: String,
    pub file_index: Option<i32>,
    pub total_files: u32,
    pub bytes_received: u64,
    pub total_bytes: u64,
    pub chunks_received: u32,
    pub total_chunks: u32,
    pub bytes_per_sec: Option<f64>,
    pub report: Option<serde_json::Value>,
}

/// Filter for `list_uploads`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UploadListFilter {
    pub user_id: Option<String>,
    pub state: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// A playlist plus its ordered tracks — what `GetPlaylist` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistWithTracks {
    pub playlist: Playlist,
    pub tracks: Vec<PlaylistTrack>,
}

/// Pagination params for list/search calls. Server caps `limit` at 200.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    pub fn new(limit: i64, offset: i64) -> Self {
        Self { limit, offset }
    }
}

impl Default for Page {
    fn default() -> Self {
        Self { limit: 50, offset: 0 }
    }
}

/// Auth credential carried per request.
///
/// `SecretKey` resolves to effective Admin on the server (see
/// `server/src/auth/identity.rs`). `Bearer` is the opaque session token
/// returned by `Login`.
#[derive(Debug, Clone)]
pub enum Credential {
    SecretKey(String),
    Bearer(String),
}

/// Coarse permission tier reported back to the client.
///
/// Mirrors `music.auth.v1.PermissionLevel` but stays decoupled from the
/// proto enum so the rest of the crate doesn't need to import generated
/// types just to read a tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionTier {
    Admin,
    Manager,
    User,
}

/// One registered user — what `ListUsers` returns. No password hash.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserEntry {
    pub id: String,
    pub username: String,
    pub level: PermissionTier,
}

/// Report from a library rescan (duration refresh).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RescanReport {
    pub tracks_checked: u64,
    pub tracks_updated: u64,
    pub errors: u64,
}

impl PermissionTier {
    /// Decode the proto enum's i32 wire value into a tier. Unknown values
    /// fall back to `User` (least privilege) on the principle that an
    /// unrecognised tier should not unlock UI affordances.
    pub fn from_proto(level: i32) -> Self {
        match level {
            3 => Self::Admin,
            2 => Self::Manager,
            _ => Self::User,
        }
    }
}
