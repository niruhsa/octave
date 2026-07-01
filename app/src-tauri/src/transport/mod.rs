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
/// One known spelling of an artist/album, preserved across merges. `name` is
/// the spelling (artist name or album title); `sort_name` is artist-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasInfo {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub language: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    /// Server-side path to a manager-uploaded artist image, or `None`.
    /// The client renders it via `GET /artists/:id/image` (proxied through
    /// the `cover://` scheme); the path itself isn't used directly.
    pub image_path: Option<String>,
    /// Every known spelling (Korean + English, etc.). Populated on
    /// single-entity reads/mutations; empty on list/search rows.
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
    /// Sum of the on-disk bytes of every track owned by this artist.
    #[serde(default)]
    pub storage_bytes: i64,
}

/// One distinct `<Language>/<Artist>` directory an artist's tracks live under
/// on the server's disk (artist storage-language consolidation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistLibraryPath {
    pub language: String,
    pub artist_folder: String,
    pub relative_dir: String,
    pub track_count: u64,
    pub storage_bytes: i64,
}

/// Response of the artist library-paths query: the directories the artist
/// occupies plus the language folders already present in the library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStoragePaths {
    #[serde(default)]
    pub paths: Vec<ArtistLibraryPath>,
    #[serde(default)]
    pub library_languages: Vec<String>,
}

/// Result of relocating an artist into a single language folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelocateReport {
    #[serde(default)]
    pub moved: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default)]
    pub target_relative_dir: String,
}

/// Server's view of an album. `cover_path` is server-relative — not yet
/// a local file; downloads land in Phase 6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i64>,
    /// Classification: `album` | `ep` | `single`.
    #[serde(default = "default_album_type")]
    pub album_type: String,
    /// True when any track on this album is explicit.
    #[serde(default)]
    pub is_explicit: bool,
    pub cover_path: Option<String>,
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
    /// Sum of the on-disk bytes of every track on this album.
    #[serde(default)]
    pub storage_bytes: i64,
}

fn default_album_type() -> String {
    "album".to_string()
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
    /// Audio-quality detail probed server-side. `None` when unknown.
    #[serde(default)]
    pub sample_rate_hz: Option<i64>,
    #[serde(default)]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub channels: Option<i64>,
    pub metadata_json: String,
    /// `true` when this track is a single release within its album.
    #[serde(default)]
    pub is_single_release: bool,
    /// `true` when this track is explicit (independent of the title text).
    #[serde(default)]
    pub is_explicit: bool,
    /// Alternate title spellings (populated on single-entity reads only).
    #[serde(default)]
    pub aliases: Vec<AliasInfo>,
}

/// The server's library-storage breakdown (homepage widget). `misc` shown in
/// the UI is `artwork_bytes + other_bytes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryStorage {
    #[serde(default)]
    pub music_bytes: i64,
    #[serde(default)]
    pub podcast_bytes: i64,
    #[serde(default)]
    pub artwork_bytes: i64,
    #[serde(default)]
    pub other_bytes: i64,
    #[serde(default)]
    pub total_bytes: i64,
    #[serde(default)]
    pub track_count: i64,
    #[serde(default)]
    pub album_count: i64,
    #[serde(default)]
    pub artist_count: i64,
    #[serde(default)]
    pub podcast_count: i64,
    #[serde(default)]
    pub episode_count: i64,
    #[serde(default)]
    pub computed_at: String,
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

// ── Follows & notifications (Phase 10) ─────────────────────────────────

/// One delivered notification (server's `Notification`). `kind` is free text
/// (`"new_release"` today). `artist_id`/`album_id` are `None` when the
/// referenced entity was since deleted; `title`/`body` are denormalized so the
/// notification still reads correctly. `read` mirrors the server's `read_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    /// Set on a `"new_episode"` notification (the podcast / episode the alert
    /// is about); `None` for music alerts.
    #[serde(default)]
    pub podcast_id: Option<String>,
    #[serde(default)]
    pub episode_id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub read: bool,
    pub created_at: String,
}

/// A page of the caller's notifications plus the total unread count (for a
/// badge). Mirrors the server's `ListNotificationsResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPage {
    pub notifications: Vec<Notification>,
    pub total: i64,
    pub unread_count: i64,
}

// ── Play history (Phase 11) ────────────────────────────────────────────

/// A play the client posts to the server. The denormalized display fields are
/// resolved server-side from `track_id`. `played_at` is RFC3339 (the time the
/// play happened); `None` lets the server stamp receipt time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayInput {
    pub track_id: String,
    pub ms_played: i64,
    pub completed: bool,
    #[serde(default)]
    pub played_at: Option<String>,
}

/// One recorded play (server's `PlayEvent`). The entity refs are `None` when
/// the catalog row was since deleted; `track_title`/`artist_name` are
/// denormalized so the row still reads correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayEvent {
    pub id: String,
    pub track_id: Option<String>,
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub ms_played: i64,
    pub completed: bool,
    pub played_at: String,
}

/// A page of the caller's plays (newest first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayHistoryPage {
    pub events: Vec<PlayEvent>,
    pub total: i64,
}

/// One "top tracks" row in a stats window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackStat {
    pub track_id: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub plays: i64,
}

/// One "top artists" row in a stats window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistStat {
    pub artist_id: Option<String>,
    pub artist_name: String,
    pub plays: i64,
}

/// Aggregate listening stats over a window (server's `GetStatsResponse`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListeningStats {
    pub top_tracks: Vec<TrackStat>,
    pub top_artists: Vec<ArtistStat>,
    pub total_plays: i64,
    pub total_ms: i64,
}

// ── Discover (Phase 11) ────────────────────────────────────────────────

/// One home shelf — a titled list of albums (server's `DiscoverSection`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverSection {
    pub id: String,
    pub title: String,
    pub albums: Vec<Album>,
}

/// Acoustic-fingerprint analysis coverage (Phase 12 — "sounds like" radio).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerprintStatus {
    pub analyzed: i64,
    pub total: i64,
    pub model_version: String,
    pub enabled: bool,
}

// ── Podcasts ───────────────────────────────────────────────────────────

/// Server's view of a podcast show. `categories` is the parsed list (the
/// server stores it as a JSON-TEXT column). `auto_download` is the per-show
/// newest-N policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Podcast {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
    pub auto_download: i32,
    pub last_refreshed_at: Option<String>,
    /// Sum of the on-disk bytes of every downloaded episode of this show.
    #[serde(default)]
    pub storage_bytes: i64,
}

/// A directory search result (enough to subscribe to a feed + display it).
/// Not an episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodcastCandidate {
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
}

/// Server's view of an episode. `downloaded` means the **server** has the audio
/// on disk (its stream endpoint will serve it); the client's own offline state
/// lives in the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodcastEpisode {
    pub id: String,
    pub podcast_id: String,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub episode_no: Option<i64>,
    pub season_no: Option<i64>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i64>,
    pub file_size: Option<i64>,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    pub downloaded: bool,
}

/// The caller's playback progress on one episode (server's view). Mirrors the
/// `EpisodeProgress` proto / REST DTO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeProgress {
    pub episode_id: String,
    pub position_ms: i64,
    pub completed: bool,
    pub updated_at: String,
}

/// Outcome of a feed refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshReport {
    pub podcast_id: String,
    pub new_episodes: i64,
    pub not_modified: bool,
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

/// A single opt-in metadata edit for one track (Phase 9). Mirrors the
/// server's `EditTrackMetadataRequest`: every field is optional and `None`
/// means "leave unchanged". `skip_serializing_if` keeps the REST PATCH body
/// to only the fields the manager actually touched.
///
/// `year` is written back to the file's audio tag only (it's not a track DB
/// column) and takes effect server-side only when `WRITE_TAGS` is enabled.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetadataEdit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_no: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_no: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
}

impl MetadataEdit {
    /// True when nothing would change — used to short-circuit a no-op edit.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.track_no.is_none()
            && self.disc_no.is_none()
            && self.metadata_json.is_none()
            && self.year.is_none()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_edit_empty_default() {
        assert!(MetadataEdit::default().is_empty());
        assert!(!MetadataEdit {
            title: Some("x".into()),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn metadata_edit_serialises_only_touched_fields() {
        // Only `title` + `track_no` set → PATCH body omits the rest, so the
        // server reads "leave unchanged" for disc_no / metadata_json / year.
        let edit = MetadataEdit {
            title: Some("New Title".into()),
            track_no: Some(3),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&edit).unwrap();
        assert_eq!(v["title"], "New Title");
        assert_eq!(v["track_no"], 3);
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("disc_no"));
        assert!(!obj.contains_key("metadata_json"));
        assert!(!obj.contains_key("year"));
    }

    #[test]
    fn metadata_edit_deserialises_from_partial_frontend_payload() {
        // The frontend sends only the fields the manager touched; absent
        // Option fields deserialise to None.
        let edit: MetadataEdit = serde_json::from_str(r#"{ "disc_no": 2 }"#).unwrap();
        assert_eq!(edit.disc_no, Some(2));
        assert!(edit.title.is_none());
        assert!(edit.year.is_none());
    }

    #[test]
    fn artist_aliases_round_trip() {
        // An artist carries its preserved spellings; the primary one is the
        // displayed name. JSON survives a round-trip into the frontend.
        let artist = Artist {
            id: "a1".into(),
            name: "YUQI".into(),
            sort_name: None,
            image_path: None,
            aliases: vec![
                AliasInfo {
                    id: "x".into(),
                    name: "YUQI".into(),
                    sort_name: None,
                    language: Some("English".into()),
                    is_primary: true,
                },
                AliasInfo {
                    id: "y".into(),
                    name: "우기 ((여자)아이들)".into(),
                    sort_name: None,
                    language: Some("Korean".into()),
                    is_primary: false,
                },
            ],
            storage_bytes: 0,
        };
        let json = serde_json::to_string(&artist).unwrap();
        let back: Artist = serde_json::from_str(&json).unwrap();
        assert_eq!(back.aliases.len(), 2);
        assert_eq!(back.name, "YUQI");
        assert!(back.aliases[0].is_primary);
        assert_eq!(back.aliases[1].name, "우기 ((여자)아이들)");
    }

    #[test]
    fn track_single_release_defaults_false_when_absent() {
        // A server payload that predates the field (or list/search rows)
        // deserialises `is_single_release` to its `#[serde(default)]` false;
        // `aliases` likewise defaults to empty.
        let t: Track = serde_json::from_str(
            r#"{ "id":"t1","album_id":"al","artist_id":"ar","title":"x",
                 "track_no":null,"disc_no":null,"duration_ms":1000,"codec":"flac",
                 "bitrate_kbps":null,"file_path":"/x.flac","file_size":null,
                 "metadata_json":"{}" }"#,
        )
        .unwrap();
        assert!(!t.is_single_release);

        let a: Album = serde_json::from_str(
            r#"{ "id":"al","artist_id":"ar","title":"X","release_year":null,
                 "cover_path":null }"#,
        )
        .unwrap();
        assert!(a.aliases.is_empty());
    }

    #[test]
    fn notification_deserialises_with_null_entity_refs() {
        // A new-release notification carries the album; one whose album was
        // since deleted comes back with null `album_id`/`artist_id`/`body`.
        let n: Notification = serde_json::from_str(
            r#"{ "id":"n1","kind":"new_release","artist_id":"ar","album_id":"al",
                 "title":"New release from BABYMETAL","body":"METAL GALAXY",
                 "read":false,"created_at":"2026-06-26T00:00:00Z" }"#,
        )
        .unwrap();
        assert_eq!(n.album_id.as_deref(), Some("al"));
        assert!(!n.read);

        let orphan: Notification = serde_json::from_str(
            r#"{ "id":"n2","kind":"new_release","artist_id":null,"album_id":null,
                 "title":"New release","body":null,"read":true,
                 "created_at":"2026-06-26T00:00:00Z" }"#,
        )
        .unwrap();
        assert!(orphan.artist_id.is_none());
        assert!(orphan.album_id.is_none());
        assert!(orphan.body.is_none());
        assert!(orphan.read);
    }

    #[test]
    fn notification_page_round_trip() {
        let page = NotificationPage {
            notifications: vec![Notification {
                id: "n1".into(),
                kind: "new_release".into(),
                artist_id: Some("ar".into()),
                album_id: Some("al".into()),
                podcast_id: None,
                episode_id: None,
                title: "New release from YUQI".into(),
                body: Some("YUQ1".into()),
                read: false,
                created_at: "2026-06-26T00:00:00Z".into(),
            }],
            total: 1,
            unread_count: 1,
        };
        let json = serde_json::to_string(&page).unwrap();
        let back: NotificationPage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total, 1);
        assert_eq!(back.unread_count, 1);
        assert_eq!(back.notifications.len(), 1);
        assert_eq!(back.notifications[0].kind, "new_release");
    }
}
