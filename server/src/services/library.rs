//! Library management: artists, albums, tracks.
//!
//! Service-layer rules (defense in depth — transport already gated):
//! - Reads: any authed identity.
//! - Mutations: `Manager+`. Every mutation writes an [`audit_log`] row.
//! - Pagination is capped (`MAX_PAGE_LIMIT`).

use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    self as m, Album, Artist, NewAlbum, NewArtist, NewAuditEntry, NewTrack, PermissionLevel, Track,
};
use crate::db::repo::{AlbumRepo, ArtistRepo, AuditRepo, TrackRepo};
use crate::error::{AppError, Result};

const MAX_PAGE_LIMIT: i64 = 200;
const DEFAULT_PAGE_LIMIT: i64 = 50;

#[derive(Clone)]
pub struct LibraryService {
    pub artists: Arc<dyn ArtistRepo>,
    pub albums: Arc<dyn AlbumRepo>,
    pub tracks: Arc<dyn TrackRepo>,
    pub audit: Arc<dyn AuditRepo>,
}

impl LibraryService {
    pub fn new(
        artists: Arc<dyn ArtistRepo>,
        albums: Arc<dyn AlbumRepo>,
        tracks: Arc<dyn TrackRepo>,
        audit: Arc<dyn AuditRepo>,
    ) -> Self {
        Self {
            artists,
            albums,
            tracks,
            audit,
        }
    }

    // -----------------------------------------------------------------------
    // Artists
    // -----------------------------------------------------------------------

    pub async fn create_artist(
        &self,
        caller: &Identity,
        name: &str,
        sort_name: Option<&str>,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        if name.trim().is_empty() {
            return Err(AppError::InvalidArgument("artist name is required".into()));
        }
        let artist = self
            .artists
            .create(NewArtist {
                name: name.to_string(),
                sort_name: sort_name.map(str::to_string),
            })
            .await?;
        self.audit(
            caller,
            "artist.create",
            "artist",
            Some(artist.id),
            None::<&m::Artist>,
            Some(&artist),
        )
        .await?;
        Ok(artist)
    }

    pub async fn get_artist(&self, caller: &Identity, id: Uuid) -> Result<Artist> {
        caller.require(PermissionLevel::User)?;
        self.artists
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {id}")))
    }

    pub async fn update_artist(
        &self,
        caller: &Identity,
        id: Uuid,
        name: &str,
        sort_name: Option<&str>,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .artists
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {id}")))?;
        let after = self
            .artists
            .update(id, name, sort_name)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {id}")))?;
        self.audit(caller, "artist.update", "artist", Some(id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn delete_artist(&self, caller: &Identity, id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::Manager)?;
        let before = self.artists.get(id).await?;
        if before.is_none() {
            return Ok(false);
        }
        self.artists.delete(id).await?;
        self.audit(caller, "artist.delete", "artist", Some(id), before.as_ref(), None::<&()>)
            .await?;
        Ok(true)
    }

    pub async fn list_artists(
        &self,
        caller: &Identity,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<(Vec<Artist>, i64)> {
        caller.require(PermissionLevel::User)?;
        let (limit, offset) = paginate(limit, offset);
        let total = self.artists.count().await?;
        let rows = self.artists.list(limit, offset).await?;
        Ok((rows, total))
    }

    pub async fn search_artists(
        &self,
        caller: &Identity,
        query: &str,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Artist>> {
        caller.require(PermissionLevel::User)?;
        if query.trim().is_empty() {
            return Err(AppError::InvalidArgument("search query is required".into()));
        }
        let (limit, offset) = paginate(limit, offset);
        self.artists.search(query, limit, offset).await
    }

    // -----------------------------------------------------------------------
    // Albums
    // -----------------------------------------------------------------------

    pub async fn create_album(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        title: &str,
        release_year: Option<i32>,
        cover_path: Option<&str>,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        if title.trim().is_empty() {
            return Err(AppError::InvalidArgument("album title is required".into()));
        }
        // Validate FK exists for a clearer error than a DB violation.
        if self.artists.get(artist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("artist {artist_id}")));
        }
        let album = self
            .albums
            .create(NewAlbum {
                artist_id,
                title: title.to_string(),
                release_year,
                cover_path: cover_path.map(str::to_string),
            })
            .await?;
        self.audit(
            caller,
            "album.create",
            "album",
            Some(album.id),
            None::<&m::Album>,
            Some(&album),
        )
        .await?;
        Ok(album)
    }

    pub async fn get_album(&self, caller: &Identity, id: Uuid) -> Result<Album> {
        caller.require(PermissionLevel::User)?;
        self.albums
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {id}")))
    }

    pub async fn update_album(
        &self,
        caller: &Identity,
        id: Uuid,
        title: &str,
        release_year: Option<i32>,
        cover_path: Option<&str>,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .albums
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {id}")))?;
        let after = self
            .albums
            .update(id, title, release_year, cover_path)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {id}")))?;
        self.audit(caller, "album.update", "album", Some(id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn delete_album(&self, caller: &Identity, id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::Manager)?;
        let before = self.albums.get(id).await?;
        if before.is_none() {
            return Ok(false);
        }
        self.albums.delete(id).await?;
        self.audit(caller, "album.delete", "album", Some(id), before.as_ref(), None::<&()>)
            .await?;
        Ok(true)
    }

    pub async fn list_albums_by_artist(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<Vec<Album>> {
        caller.require(PermissionLevel::User)?;
        self.albums.list_by_artist(artist_id).await
    }

    pub async fn search_albums(
        &self,
        caller: &Identity,
        query: &str,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Album>> {
        caller.require(PermissionLevel::User)?;
        if query.trim().is_empty() {
            return Err(AppError::InvalidArgument("search query is required".into()));
        }
        let (limit, offset) = paginate(limit, offset);
        self.albums.search(query, limit, offset).await
    }

    // -----------------------------------------------------------------------
    // Tracks
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_track(&self, caller: &Identity, new: NewTrack) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        if new.title.trim().is_empty() {
            return Err(AppError::InvalidArgument("track title is required".into()));
        }
        if new.file_path.trim().is_empty() {
            return Err(AppError::InvalidArgument("file_path is required".into()));
        }
        if self.albums.get(new.album_id).await?.is_none() {
            return Err(AppError::NotFound(format!("album {}", new.album_id)));
        }
        if self.artists.get(new.artist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("artist {}", new.artist_id)));
        }
        let track = self.tracks.create(new).await?;
        self.audit(
            caller,
            "track.create",
            "track",
            Some(track.id),
            None::<&m::Track>,
            Some(&track),
        )
        .await?;
        Ok(track)
    }

    pub async fn get_track(&self, caller: &Identity, id: Uuid) -> Result<Track> {
        caller.require(PermissionLevel::User)?;
        self.tracks
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {id}")))
    }

    pub async fn update_track(
        &self,
        caller: &Identity,
        id: Uuid,
        title: &str,
        track_no: Option<i32>,
        disc_no: Option<i32>,
        metadata_json: &str,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {id}")))?;
        let after = self
            .tracks
            .update(id, title, track_no, disc_no, metadata_json)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {id}")))?;
        self.audit(caller, "track.update", "track", Some(id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn delete_track(&self, caller: &Identity, id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::Manager)?;
        let before = self.tracks.get(id).await?;
        if before.is_none() {
            return Ok(false);
        }
        self.tracks.delete(id).await?;
        self.audit(caller, "track.delete", "track", Some(id), before.as_ref(), None::<&()>)
            .await?;
        Ok(true)
    }

    pub async fn list_tracks_by_album(
        &self,
        caller: &Identity,
        album_id: Uuid,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;
        self.tracks.list_by_album(album_id).await
    }

    pub async fn search_tracks(
        &self,
        caller: &Identity,
        query: &str,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;
        if query.trim().is_empty() {
            return Err(AppError::InvalidArgument("search query is required".into()));
        }
        let (limit, offset) = paginate(limit, offset);
        self.tracks.search(query, limit, offset).await
    }

    // -----------------------------------------------------------------------
    // Audit helper
    // -----------------------------------------------------------------------

    async fn audit<B: Serialize, A: Serialize>(
        &self,
        caller: &Identity,
        action: &str,
        entity_type: &str,
        entity_id: Option<Uuid>,
        before: Option<&B>,
        after: Option<&A>,
    ) -> Result<()> {
        let before_json = match before {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| AppError::Internal(format!("audit json: {e}")))?,
            ),
            None => None,
        };
        let after_json = match after {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| AppError::Internal(format!("audit json: {e}")))?,
            ),
            None => None,
        };
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: action.to_string(),
                entity_type: entity_type.to_string(),
                entity_id,
                before_json,
                after_json,
            })
            .await?;
        Ok(())
    }
}

fn paginate(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
    let limit = limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    let offset = offset.unwrap_or(0).max(0);
    (limit, offset)
}
