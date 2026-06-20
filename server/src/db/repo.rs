//! Repository traits.
//!
//! Each entity has a narrow async trait so callers can be unit-tested against
//! an in-memory fake while the Postgres impls in [`super::pg`] back production.

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Result;

use super::models::*;

#[async_trait]
pub trait UserRepo: Send + Sync {
    async fn create(&self, new: NewUser) -> Result<User>;
    async fn get(&self, id: Uuid) -> Result<Option<User>>;
    async fn find_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn update_permission(&self, id: Uuid, level: PermissionLevel) -> Result<()>;
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
