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

/// Which catalog entity a favorite points at (Phase 11). Selects the column in
/// the polymorphic `favorites` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteKind {
    Track,
    Album,
    Artist,
}

impl FavoriteKind {
    /// Parse a wire string (`"track"`/`"album"`/`"artist"`).
    pub fn parse(s: &str) -> Option<FavoriteKind> {
        match s.to_ascii_lowercase().as_str() {
            "track" => Some(FavoriteKind::Track),
            "album" => Some(FavoriteKind::Album),
            "artist" => Some(FavoriteKind::Artist),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FavoriteKind::Track => "track",
            FavoriteKind::Album => "album",
            FavoriteKind::Artist => "artist",
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
    /// Sum of the on-disk bytes of every track owned by this artist. Kept up
    /// to date by `StorageService::recompute_aggregates`.
    pub storage_bytes: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Album {
    pub id: Uuid,
    pub artist_id: Uuid,
    pub title: String,
    pub release_year: Option<i32>,
    /// One of `album` / `ep` / `single` / `live`. A `single` album always has
    /// at least one track flagged `is_single_release` (enforced in
    /// `LibraryService`); the others are unrestricted.
    pub album_type: String,
    /// `true` when any track on this album is explicit (denormalized rollup,
    /// recomputed by `LibraryService`).
    pub is_explicit: bool,
    pub cover_path: Option<String>,
    /// Sum of the on-disk bytes of every track on this album.
    pub storage_bytes: i64,
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
    /// Audio-quality detail probed at ingest/rescan (sample rate in Hz, bit
    /// depth, channel count). Nullable — unknown until probed, and bit depth in
    /// particular is often absent for lossy formats.
    pub sample_rate_hz: Option<i32>,
    pub bit_depth: Option<i32>,
    pub channels: Option<i32>,
    /// JSON-as-TEXT; validated at the service layer.
    pub metadata_json: String,
    /// `true` when this track is a "single release" within its album — e.g.
    /// it was moved in from a one-track single album via `move_track`.
    pub is_single_release: bool,
    /// `true` when this track is explicit (independent of the title text).
    pub is_explicit: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One known spelling of an artist. Every artist has at least one (the primary,
/// mirrored into `artists.name`); merging duplicates adds the merged-in
/// spellings here so nothing is lost. `language` is the inferred/declared label
/// (e.g. `"English"`, `"Korean"`); `None` means "infer from the script".
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct ArtistAlias {
    pub id: Uuid,
    pub artist_id: Uuid,
    pub name: String,
    pub sort_name: Option<String>,
    pub language: Option<String>,
    pub is_primary: bool,
    pub created_at: OffsetDateTime,
}

/// One known spelling of an album title (see [`ArtistAlias`]).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct AlbumAlias {
    pub id: Uuid,
    pub album_id: Uuid,
    pub title: String,
    pub language: Option<String>,
    pub is_primary: bool,
    pub created_at: OffsetDateTime,
}

/// One known spelling of a track title (see [`AlbumAlias`]).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct TrackAlias {
    pub id: Uuid,
    pub track_id: Uuid,
    pub title: String,
    pub language: Option<String>,
    pub is_primary: bool,
    pub created_at: OffsetDateTime,
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

/// A delivered notification (Phase 10). One row per recipient. `kind` is free
/// TEXT (`"new_release"` for a followed artist's album; `"new_episode"` for a
/// subscribed podcast's episode). `artist_id`/`album_id` (music) and
/// `podcast_id`/`episode_id` (podcasts) are nullable (they go NULL if the
/// entity is later deleted); the denormalized `title`/`body` keep the
/// notification readable regardless. `read_at` NULL means unread.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub artist_id: Option<Uuid>,
    pub album_id: Option<Uuid>,
    pub podcast_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    pub title: String,
    pub body: Option<String>,
    pub read_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// A recorded play (Phase 11 — play history). One row per "play" event posted
/// by the client (which decides what counts — e.g. ≥30 s or ≥50 % of the
/// track). `track_id`/`artist_id`/`album_id` go NULL if the catalog row is
/// later deleted; the denormalized `track_title`/`artist_name` keep the row
/// readable regardless. `ms_played`/`completed` distinguish a skip from a real
/// listen. `played_at` is client-supplied so offline plays keep their real time.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PlayEvent {
    pub id: Uuid,
    pub user_id: Uuid,
    pub track_id: Option<Uuid>,
    pub artist_id: Option<Uuid>,
    pub album_id: Option<Uuid>,
    pub track_title: String,
    pub artist_name: String,
    pub ms_played: i64,
    pub completed: bool,
    pub played_at: OffsetDateTime,
}

/// An acoustic similarity embedding for one track (Phase 12 — "sounds like"
/// radio). Server-only — deliberately **not** mirrored into the client SQLite
/// cache (embeddings are large and irrelevant offline). The vector is stored as
/// a raw little-endian f32 `BYTEA` blob and decoded to `Vec<f32>` at the repo
/// boundary; `dims` + `model_version` make a row self-describing so a model bump
/// can re-analyze only what's stale. `source_sig` is the file-content signature
/// (size+mtime) at analysis time so a re-encoded file is re-analyzed.
#[derive(Debug, Clone)]
pub struct TrackFeature {
    pub track_id: Uuid,
    pub embedding: Vec<f32>,
    pub dims: i32,
    pub model_version: String,
    pub source_sig: String,
    pub chromaprint: Option<String>,
    pub analyzed_at: OffsetDateTime,
}

/// Insert/upsert shape for a [`TrackFeature`]. `analyzed_at` is DB-defaulted.
#[derive(Debug, Clone)]
pub struct NewTrackFeature {
    pub track_id: Uuid,
    pub embedding: Vec<f32>,
    pub dims: i32,
    pub model_version: String,
    pub source_sig: String,
    pub chromaprint: Option<String>,
}

/// Lightweight `(track_id, source_sig, model_version)` row for the "needs
/// analysis?" scan — avoids loading every embedding blob just to decide
/// freshness.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrackFeatureStatus {
    pub track_id: Uuid,
    pub source_sig: String,
    pub model_version: String,
}

/// A registered device push token (Phase 10 — FCM). `token` is the FCM
/// registration token; `platform` is `"android"` today. Owned by a user; the
/// new-release fan-out pushes to every token of each follower.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct DeviceToken {
    pub token: String,
    pub user_id: Uuid,
    pub platform: String,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
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

/// The singleton library-storage breakdown row (`library_storage`, `id = 1`).
/// `music_bytes`/`podcast_bytes` are SQL sums of the respective file sizes;
/// `artwork_bytes`/`other_bytes` come from a filesystem walk. The UI shows
/// `misc = artwork_bytes + other_bytes`. Recomputed on scan/upload and by the
/// 24h background job. See [`crate::services::storage::StorageService`].
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct LibraryStorage {
    pub music_bytes: i64,
    pub podcast_bytes: i64,
    pub artwork_bytes: i64,
    pub other_bytes: i64,
    pub total_bytes: i64,
    pub track_count: i64,
    pub album_count: i64,
    pub artist_count: i64,
    pub podcast_count: i64,
    pub episode_count: i64,
    pub computed_at: OffsetDateTime,
}

/// SQL-derived aggregates (no filesystem walk) — the cheap recompute path.
#[derive(Debug, Clone, Copy, Default, sqlx::FromRow)]
pub struct StorageAggregates {
    pub music_bytes: i64,
    pub podcast_bytes: i64,
    pub track_count: i64,
    pub album_count: i64,
    pub artist_count: i64,
    pub podcast_count: i64,
    pub episode_count: i64,
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
    pub sample_rate_hz: Option<i32>,
    pub bit_depth: Option<i32>,
    pub channels: Option<i32>,
    pub metadata_json: String,
}

#[derive(Debug, Clone)]
pub struct NewArtistAlias {
    pub artist_id: Uuid,
    pub name: String,
    pub sort_name: Option<String>,
    pub language: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone)]
pub struct NewAlbumAlias {
    pub album_id: Uuid,
    pub title: String,
    pub language: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone)]
pub struct NewTrackAlias {
    pub track_id: Uuid,
    pub title: String,
    pub language: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone)]
pub struct NewPlaylist {
    pub owner_id: Uuid,
    pub name: String,
}

/// Insert-shape for a notification. `id`/`read_at`/`created_at` are set by the
/// DB (the row starts unread). `artist_id`/`album_id` carry a music alert;
/// `podcast_id`/`episode_id` carry a podcast alert — set whichever pair fits
/// the `kind`, leave the other `None`.
#[derive(Debug, Clone, Default)]
pub struct NewNotification {
    pub user_id: Uuid,
    pub kind: String,
    pub artist_id: Option<Uuid>,
    pub album_id: Option<Uuid>,
    pub podcast_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    pub title: String,
    pub body: Option<String>,
}

/// Insert-shape for a play event. `id` is DB-generated. The denormalized
/// display fields + `artist_id`/`album_id` are resolved server-side from the
/// `track_id` at record time (the client posts only `track_id`/`ms_played`/
/// `completed`/`played_at`). `played_at` defaults to now() in the DB when `None`.
#[derive(Debug, Clone)]
pub struct NewPlayEvent {
    pub user_id: Uuid,
    pub track_id: Uuid,
    pub artist_id: Uuid,
    pub album_id: Uuid,
    pub track_title: String,
    pub artist_name: String,
    pub ms_played: i64,
    pub completed: bool,
    pub played_at: Option<OffsetDateTime>,
}

/// Insert/upsert shape for a device push token.
#[derive(Debug, Clone)]
pub struct NewDeviceToken {
    pub token: String,
    pub user_id: Uuid,
    pub platform: String,
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
/// `initialized` → `uploading` → `completed`, or `cancelled` from any active
/// state. `uploading` ⇄ `paused` (manual pause/resume, or an auto-pause when a
/// client's chunk uploads stall/fail for ≥1 min; a chunk landing resumes it).
/// Per-chunk hash failures don't advance state (the chunk POST just fails);
/// whole-file/ingest errors are captured in the completion report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UploadState {
    Initialized,
    Uploading,
    Paused,
    Completed,
    Cancelled,
}

impl UploadState {
    /// The states in which an upload is still in flight (counts toward the
    /// one-active-upload-per-user limit; cancellable; accepts chunks). `paused`
    /// is active — it's resumable and a chunk landing transitions it back to
    /// `uploading`.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            UploadState::Initialized | UploadState::Uploading | UploadState::Paused
        )
    }

    /// Parse a wire/query string into a state, for `?state=` filters.
    pub fn parse(s: &str) -> Option<UploadState> {
        match s.to_ascii_lowercase().as_str() {
            "initialized" => Some(UploadState::Initialized),
            "uploading" => Some(UploadState::Uploading),
            "paused" => Some(UploadState::Paused),
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

// ---------------------------------------------------------------------------
// Podcasts: a catalog show (like an artist) whose episodes are on-disk audio
// files (like tracks). Episodes stream through the same byte-range path; new
// episodes reuse the notification fan-out.
// ---------------------------------------------------------------------------

/// A subscribed podcast show (one RSS feed). `feed_url` is the natural key;
/// `categories` is a JSON array stored as TEXT (portable to SQLite). `auto_download`
/// is the per-show newest-N policy (0 = metadata only). `image_path` is the
/// on-disk cached cover (like `albums.cover_path`); `last_etag`/`last_modified`
/// back conditional feed GETs.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Podcast {
    pub id: Uuid,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_path: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    /// JSON array as TEXT (e.g. `["News","Technology"]`).
    pub categories: String,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
    pub auto_download: i32,
    /// Sum of the on-disk bytes of every downloaded episode of this show.
    pub storage_bytes: i64,
    pub last_refreshed_at: Option<OffsetDateTime>,
    pub last_etag: Option<String>,
    pub last_modified: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One episode (`<item>`) of a podcast. Mirrors `Track`: `file_path` is `None`
/// until the audio is downloaded to disk, at which point it streams exactly
/// like a track. `guid` is the feed's episode identity (unique per podcast).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PodcastEpisode {
    pub id: Uuid,
    pub podcast_id: Uuid,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub episode_no: Option<i32>,
    pub season_no: Option<i32>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i32>,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub image_path: Option<String>,
    pub published_at: Option<OffsetDateTime>,
    pub metadata_json: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A user's subscription to a podcast (for new-episode notifications). Mirrors
/// [`Follow`].
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct PodcastSubscription {
    pub user_id: Uuid,
    pub podcast_id: Uuid,
    pub created_at: OffsetDateTime,
}

/// A user's playback progress on one episode: how far in they are (`position_ms`)
/// and whether they've finished it (`completed`). Drives "continue where you left
/// off" and the listened/in-progress markers in the episode list.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct EpisodeProgress {
    pub episode_id: Uuid,
    pub position_ms: i64,
    pub completed: bool,
    pub updated_at: OffsetDateTime,
}

/// Upsert-shape for a podcast (the feed-derived metadata). `id`/refresh
/// bookkeeping/`image_path` are owned by the DB / service, not the feed.
#[derive(Debug, Clone, Default)]
pub struct NewPodcast {
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    pub categories: String,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
    pub auto_download: i32,
}

/// Upsert-shape for an episode (the feed-derived fields). Technical fields
/// (`codec`/`bitrate`/`file_path`/`file_size`) are filled on download.
#[derive(Debug, Clone, Default)]
pub struct NewPodcastEpisode {
    pub podcast_id: Uuid,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub episode_no: Option<i32>,
    pub season_no: Option<i32>,
    pub duration_ms: Option<i64>,
    pub image_path: Option<String>,
    pub published_at: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------
// Discography sync (Phase 14 — external metadata reconciliation).
//
// Resolution state + reports live in side tables keyed by artist_id (see the
// 20270801 migration) so the shared Artist/Album structs — and their many
// hand-written queries — stay untouched. Server-only; not mirrored to SQLite.
// ---------------------------------------------------------------------------

/// Per-artist provider resolution state (Phase D — provider-agnostic).
/// `provider` names the metadata source and `provider_id` is the artist's id on
/// that provider (MusicBrainz MBID, Discogs artist id, …); both sticky once set.
/// `match_status` ∈ `unresolved` / `matched` / `manual` / `ignored`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct ArtistDiscoState {
    pub artist_id: Uuid,
    pub provider: Option<String>,
    pub provider_id: Option<String>,
    pub match_status: String,
    pub synced_at: Option<OffsetDateTime>,
}

/// A release (album/EP/single/live) the library is missing entirely. `year` is
/// the provider's first-release year; `provider_id` is the release-group MBID
/// (a string on the wire; a client echoes it back to ignore the release).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingRelease {
    pub title: String,
    /// Mapped `album_type`: `album` / `ep` / `single` / `live`.
    pub album_type: String,
    pub year: Option<i32>,
    pub provider_id: String,
}

/// An owned album that is missing one or more tracks vs. the provider's
/// canonical edition. `release_group_id` (the matched release-group MBID) lets a
/// client scope a track ignore to this release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IncompleteAlbum {
    pub album_id: Uuid,
    pub title: String,
    pub release_group_id: String,
    pub missing_tracks: Vec<MissingTrack>,
}

/// One track missing from an owned album. `recording_id` (recording MBID, when
/// the provider has one) + `title_key` (normalized title) are the ignore keys a
/// client echoes back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingTrack {
    pub title: String,
    pub position: Option<i32>,
    pub disc_no: Option<i32>,
    pub recording_id: Option<String>,
    pub title_key: String,
}

/// The cached gap report served to the UI (assembled from the stored JSON in
/// `discography_reports`, after suppression filtering).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscographyReport {
    pub artist_id: Uuid,
    pub provider: String,
    pub missing_releases: Vec<MissingRelease>,
    pub incomplete_albums: Vec<IncompleteAlbum>,
    pub missing_release_count: i32,
    pub incomplete_album_count: i32,
    pub generated_at: OffsetDateTime,
}

/// Raw report row as persisted: the three JSON payloads (as TEXT) + counts. The
/// service deserializes `missing_releases`/`incomplete_albums` for the wire and
/// `provider_snapshot` only when re-filtering after an ignore change.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredReport {
    pub provider: String,
    pub missing_releases: String,
    pub incomplete_albums: String,
    pub provider_snapshot: String,
    pub missing_release_count: i32,
    pub incomplete_album_count: i32,
    pub generated_at: OffsetDateTime,
}

/// Insert/upsert shape for a stored report.
#[derive(Debug, Clone)]
pub struct NewStoredReport {
    pub artist_id: Uuid,
    pub provider: String,
    pub missing_releases: String,
    pub incomplete_albums: String,
    pub provider_snapshot: String,
    pub missing_release_count: i32,
    pub incomplete_album_count: i32,
}

/// A suppression entry — a release or a track a manager has chosen to ignore
/// (DISCOGRAPHY_SYNC.md §4.7). Keyed on provider ids so it survives library
/// edits + re-matching.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct DiscographyIgnore {
    pub id: Uuid,
    pub artist_id: Uuid,
    /// `release` or `track`.
    pub scope: String,
    /// Provider release-group id (TEXT — provider-agnostic).
    pub release_group_id: String,
    /// Provider recording id (track scope), when the provider supplies one.
    pub recording_id: Option<String>,
    pub title_key: Option<String>,
    pub label: String,
    pub created_at: OffsetDateTime,
}

/// A track's Chromaprint identification fingerprint + duration, for Phase-E
/// audio-anchored resolution (AcoustID lookup). Only rows with a stored
/// `chromaprint` (the `chromaprint` build feature) are returned.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrackFingerprint {
    pub chromaprint: String,
    pub duration_ms: i64,
}

/// Insert shape for a [`DiscographyIgnore`]. `id`/`created_at` are DB-set; the
/// insert is idempotent (`ON CONFLICT DO NOTHING`).
#[derive(Debug, Clone)]
pub struct NewDiscographyIgnore {
    pub artist_id: Uuid,
    pub scope: String,
    pub release_group_id: String,
    pub recording_id: Option<String>,
    pub title_key: Option<String>,
    pub label: String,
    pub created_by: Option<Uuid>,
}
