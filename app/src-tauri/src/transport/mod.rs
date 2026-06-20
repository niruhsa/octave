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

pub use client::{LoginOutcome, ServerClient, WhoAmI};
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
