//! Repository traits.
//!
//! Each entity has a narrow async trait so callers can be unit-tested against
//! an in-memory fake while the Postgres impls in [`super::pg`] back production.

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::Result;

use super::models::*;

#[async_trait]
pub trait UserRepo: Send + Sync {
    async fn create(&self, new: NewUser) -> Result<User>;
    async fn get(&self, id: Uuid) -> Result<Option<User>>;
    async fn find_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn list(&self) -> Result<Vec<User>>;
    async fn update_permission(&self, id: Uuid, level: PermissionLevel) -> Result<()>;
    async fn update_password(&self, id: Uuid, password_hash: &str) -> Result<()>;
    async fn delete(&self, id: Uuid) -> Result<()>;
}

#[async_trait]
pub trait ArtistRepo: Send + Sync {
    async fn create(&self, new: NewArtist) -> Result<Artist>;
    async fn get(&self, id: Uuid) -> Result<Option<Artist>>;
    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<Artist>>;
    async fn count(&self) -> Result<i64>;
    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Artist>>;
    async fn update(&self, id: Uuid, name: &str, sort_name: Option<&str>) -> Result<Option<Artist>>;
    /// Set (or clear, with `None`) the artist's image path. Leaves name /
    /// sort_name untouched, so it composes with `update`.
    async fn set_image(&self, id: Uuid, image_path: Option<&str>) -> Result<Option<Artist>>;
    /// `(id, image_path)` for every artist that has an image set. Used by the
    /// image-optimization pass.
    async fn all_image_paths(&self) -> Result<Vec<(Uuid, String)>>;
    async fn find_by_name(&self, name: &str) -> Result<Option<Artist>>;
    async fn delete(&self, id: Uuid) -> Result<()>;
}

#[async_trait]
pub trait AlbumRepo: Send + Sync {
    async fn create(&self, new: NewAlbum) -> Result<Album>;
    async fn get(&self, id: Uuid) -> Result<Option<Album>>;
    async fn list_by_artist(&self, artist_id: Uuid) -> Result<Vec<Album>>;
    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Album>>;
    async fn update(
        &self,
        id: Uuid,
        title: &str,
        release_year: Option<i32>,
        cover_path: Option<&str>,
    ) -> Result<Option<Album>>;
    async fn find_by_artist_and_title(
        &self,
        artist_id: Uuid,
        title: &str,
    ) -> Result<Option<Album>>;
    /// `(id, cover_path)` for every album that has a cover set. Used by the
    /// image-optimization pass.
    async fn all_cover_paths(&self) -> Result<Vec<(Uuid, String)>>;
    /// Re-point every album owned by `from_artist` onto `to_artist`. Used when
    /// merging a duplicate artist into a survivor. Returns the number moved.
    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<u64>;
    async fn delete(&self, id: Uuid) -> Result<()>;
}

#[async_trait]
pub trait TrackRepo: Send + Sync {
    async fn create(&self, new: NewTrack) -> Result<Track>;
    async fn get(&self, id: Uuid) -> Result<Option<Track>>;
    async fn list_by_album(&self, album_id: Uuid) -> Result<Vec<Track>>;
    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Track>>;
    async fn update(
        &self,
        id: Uuid,
        title: &str,
        track_no: Option<i32>,
        disc_no: Option<i32>,
        metadata_json: &str,
    ) -> Result<Option<Track>>;
    async fn find_by_file_path(&self, file_path: &str) -> Result<Option<Track>>;
    async fn delete(&self, id: Uuid) -> Result<()>;
    /// Re-point every track owned by `from_artist` onto `to_artist` (artist
    /// merge). Returns the number moved.
    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<u64>;
    /// Re-point every track in `from_album` onto `to_album` (album merge).
    /// Returns the number moved.
    async fn reassign_album(&self, from_album: Uuid, to_album: Uuid) -> Result<u64>;
    /// Move a single track to `album_id` (the "single release" move). Returns
    /// the updated row.
    async fn set_album(&self, id: Uuid, album_id: Uuid) -> Result<Option<Track>>;
    /// Set (or clear) the single-release flag on a track. Returns the updated row.
    async fn set_single_release(&self, id: Uuid, is_single_release: bool) -> Result<Option<Track>>;
    /// Return every track's (id, file_path, duration_ms) for bulk rescan.
    async fn list_all_ids_paths(&self) -> Result<Vec<TrackIdPath>>;
    /// Overwrite the duration of a single track.  Returns the updated row.
    async fn update_duration(&self, id: Uuid, duration_ms: i64) -> Result<Option<Track>>;
    /// Refresh the file-derived technical fields (codec, bitrate, size)
    /// during a full library rescan.  Returns the updated row.
    async fn update_file_props(
        &self,
        id: Uuid,
        codec: &str,
        bitrate_kbps: Option<i32>,
        file_size: Option<i64>,
    ) -> Result<Option<Track>>;
}

/// Lightweight row for `list_all_ids_paths` — avoids fetching every column.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrackIdPath {
    pub id: Uuid,
    pub file_path: String,
    pub duration_ms: i64,
}

#[async_trait]
pub trait PlaylistRepo: Send + Sync {
    async fn create(&self, new: NewPlaylist) -> Result<Playlist>;
    async fn get(&self, id: Uuid) -> Result<Option<Playlist>>;
    async fn list_for_owner(&self, owner_id: Uuid) -> Result<Vec<Playlist>>;
    async fn update_name(&self, id: Uuid, name: &str) -> Result<Option<Playlist>>;
    async fn delete(&self, id: Uuid) -> Result<()>;

    /// Insert a track at `position`, shifting subsequent rows up by one.
    /// Use [`next_position`] + this to append at the end.
    async fn insert_track_at(
        &self,
        playlist_id: Uuid,
        track_id: Uuid,
        position: i32,
    ) -> Result<PlaylistTrack>;
    /// Remove the row at `position` and shift later rows down by one.
    /// Returns `true` if a row was removed.
    async fn remove_track_at(&self, playlist_id: Uuid, position: i32) -> Result<bool>;
    /// Move the row currently at `from` to `to`, shifting the in-between rows.
    /// Returns `true` if the row existed and was moved (or no-op when `from == to`).
    async fn move_track(&self, playlist_id: Uuid, from: i32, to: i32) -> Result<bool>;
    async fn list_tracks(&self, playlist_id: Uuid) -> Result<Vec<PlaylistTrack>>;
    /// Position to use when appending (`max(position) + 1`, or 1 when empty).
    async fn next_position(&self, playlist_id: Uuid) -> Result<i32>;
    async fn get_track_at(
        &self,
        playlist_id: Uuid,
        position: i32,
    ) -> Result<Option<PlaylistTrack>>;
}

#[async_trait]
pub trait FollowRepo: Send + Sync {
    async fn follow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()>;
    async fn unfollow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()>;
    async fn followers_of(&self, artist_id: Uuid) -> Result<Vec<Uuid>>;
    async fn following(&self, user_id: Uuid) -> Result<Vec<Uuid>>;
    /// Move every follow of `from_artist` onto `to_artist`, de-duplicating any
    /// user who already follows both (artist merge).
    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<()>;
}

/// Alias rows preserve every known spelling of an artist / album so a merge
/// never loses the original name. See [`ArtistAlias`] / [`AlbumAlias`].
#[async_trait]
pub trait AliasRepo: Send + Sync {
    // ----- Artist aliases -----
    async fn list_artist_aliases(&self, artist_id: Uuid) -> Result<Vec<ArtistAlias>>;
    /// Insert (or return the existing row, on a `(artist_id, name)` conflict)
    /// an alias. The conflict path leaves the stored row untouched.
    async fn add_artist_alias(&self, new: NewArtistAlias) -> Result<ArtistAlias>;
    async fn get_artist_alias(&self, id: Uuid) -> Result<Option<ArtistAlias>>;
    async fn delete_artist_alias(&self, id: Uuid) -> Result<()>;
    /// Mark `alias_id` primary and clear the flag on every other alias of the
    /// same artist (single primary per artist).
    async fn set_primary_artist_alias(&self, artist_id: Uuid, alias_id: Uuid) -> Result<()>;
    /// Move every alias of `from_artist` onto `to_artist`, skipping names that
    /// already exist on the target (artist merge). Reassigned aliases are no
    /// longer primary (the survivor keeps its own primary).
    async fn reassign_artist_aliases(&self, from_artist: Uuid, to_artist: Uuid) -> Result<()>;

    // ----- Album aliases -----
    async fn list_album_aliases(&self, album_id: Uuid) -> Result<Vec<AlbumAlias>>;
    async fn add_album_alias(&self, new: NewAlbumAlias) -> Result<AlbumAlias>;
    async fn get_album_alias(&self, id: Uuid) -> Result<Option<AlbumAlias>>;
    async fn delete_album_alias(&self, id: Uuid) -> Result<()>;
    async fn set_primary_album_alias(&self, album_id: Uuid, alias_id: Uuid) -> Result<()>;
    async fn reassign_album_aliases(&self, from_album: Uuid, to_album: Uuid) -> Result<()>;
}

/// Per-user notifications (Phase 10 — new-release alerts). Delivery is
/// persist-then-fetch: the new-release fan-out inserts one row per follower,
/// and clients poll [`list_for_user`](NotificationRepo::list_for_user).
#[async_trait]
pub trait NotificationRepo: Send + Sync {
    /// Insert a single notification, returning the stored row.
    async fn create(&self, new: NewNotification) -> Result<Notification>;
    /// Bulk-insert (new-release fan-out to every follower). Returns the number
    /// of rows inserted. A no-op (returns 0) on an empty slice.
    async fn create_many(&self, items: &[NewNotification]) -> Result<u64>;
    async fn get(&self, id: Uuid) -> Result<Option<Notification>>;
    /// Newest-first page for a user. `unread_only` restricts to `read_at IS NULL`.
    async fn list_for_user(
        &self,
        user_id: Uuid,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Notification>>;
    async fn unread_count(&self, user_id: Uuid) -> Result<i64>;
    /// Mark one notification read (idempotent — preserves the first read time).
    /// Scoped to `user_id` so a caller can't touch another user's row. Returns
    /// `true` when a previously-unread row was flipped.
    async fn mark_read(&self, user_id: Uuid, id: Uuid) -> Result<bool>;
    /// Mark every unread notification for a user read. Returns the count flipped.
    async fn mark_all_read(&self, user_id: Uuid) -> Result<u64>;
}

#[async_trait]
pub trait AuditRepo: Send + Sync {
    async fn record(&self, entry: NewAuditEntry) -> Result<AuditEntry>;
    async fn list_for_entity(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> Result<Vec<AuditEntry>>;
}

#[async_trait]
pub trait SessionRepo: Send + Sync {
    async fn create(&self, new: NewSession) -> Result<Session>;
    async fn get(&self, token: &str) -> Result<Option<Session>>;
    async fn revoke(&self, token: &str) -> Result<()>;
}

#[async_trait]
pub trait UploadRepo: Send + Sync {
    // ----- Sessions -----
    async fn create_upload(&self, new: NewUpload) -> Result<Upload>;
    async fn get_upload(&self, id: Uuid) -> Result<Option<Upload>>;
    async fn list_uploads(
        &self,
        filter: UploadFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Upload>>;
    /// Number of in-flight (`initialized`/`uploading`) sessions for an owner.
    /// `None` matches the `SECRET_KEY` (NULL-owner) bucket.
    async fn count_active_for_user(&self, user_id: Option<Uuid>) -> Result<i64>;
    async fn set_upload_state(&self, id: Uuid, state: UploadState) -> Result<()>;
    /// Atomically mark every active (`initialized`/`uploading`) upload whose most
    /// recent activity (latest chunk receipt, else creation — and never older
    /// than its own `updated_at`, so a fresh resume isn't re-paused) predates
    /// `cutoff` as `paused`, returning the affected sessions for event
    /// publishing. The server-side backstop for a stalled client that can't
    /// send a `pause` itself (the usual stall cause — the network is down —
    /// fails that call too).
    async fn pause_stale_active(&self, cutoff: OffsetDateTime) -> Result<Vec<Upload>>;
    /// Terminal write: state + optional aggregated report + optional error.
    async fn set_upload_report(
        &self,
        id: Uuid,
        state: UploadState,
        report_json: Option<&str>,
        error: Option<&str>,
    ) -> Result<()>;

    // ----- Files -----
    async fn create_file(&self, new: NewUploadFile) -> Result<UploadFile>;
    async fn get_file(&self, upload_id: Uuid, file_index: i32) -> Result<Option<UploadFile>>;
    async fn list_files(&self, upload_id: Uuid) -> Result<Vec<UploadFile>>;
    async fn set_file_state(
        &self,
        file_id: Uuid,
        state: UploadFileState,
        error: Option<&str>,
    ) -> Result<()>;
    /// Overwrite a file's stored filename — used after ingest to replace the
    /// name declared at init (possibly an opaque Android content-URI id) with
    /// the organised on-disk filename.
    async fn set_file_filename(&self, file_id: Uuid, filename: &str) -> Result<()>;

    // ----- Chunks -----
    async fn create_chunk(&self, new: NewUploadChunk) -> Result<()>;
    async fn list_chunks(&self, file_id: Uuid) -> Result<Vec<UploadChunk>>;
    async fn get_chunk(&self, file_id: Uuid, chunk_index: i32) -> Result<Option<UploadChunk>>;
    /// Idempotently mark a chunk received and recompute the file's
    /// `received_chunks` from the chunk table (robust to retries / races).
    /// Returns the file's `(received_chunks, total_chunks)` after the update.
    async fn mark_chunk_received(&self, file_id: Uuid, chunk_index: i32) -> Result<(i32, i32)>;
}
