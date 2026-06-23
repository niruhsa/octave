//! Library management: artists, albums, tracks.
//!
//! Service-layer rules (defense in depth — transport already gated):
//! - Reads: any authed identity.
//! - Mutations: `Manager+`. Every mutation writes an [`audit_log`] row.
//! - Pagination is capped (`MAX_PAGE_LIMIT`).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tracing::warn;
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
    /// Root directory for the organised library.  When set, file deletions
    /// resolve relative `file_path` values against this root and remove the
    /// on-disk file.  Absolute `file_path` values are used as-is.
    pub library_root: Option<PathBuf>,
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
            library_root: None,
        }
    }

    /// Set the library root for file-deletion support.
    pub fn with_library_root(mut self, root: Option<PathBuf>) -> Self {
        self.library_root = root;
        self
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

    /// Set (or clear) the artist's `image_path` (audited like any mutation).
    /// Manager+. Leaves name / sort_name untouched.
    pub async fn update_artist_image(
        &self,
        caller: &Identity,
        id: Uuid,
        image_path: Option<&str>,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .artists
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {id}")))?;
        let after = self
            .artists
            .set_image(id, image_path)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {id}")))?;
        self.audit(caller, "artist.image_update", "artist", Some(id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn delete_artist(&self, caller: &Identity, id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::Manager)?;
        let before = self.artists.get(id).await?;
        if before.is_none() {
            return Ok(false);
        }

        // Cascade: delete every album → its tracks → then the artist.
        let albums = self.albums.list_by_artist(id).await?;
        for album in &albums {
            // Reuse delete_album which cascades to tracks.
            self.delete_album(caller, album.id).await?;
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

        // Cascade: delete every track in this album, then the album.
        let tracks = self.tracks.list_by_album(id).await?;
        for track in &tracks {
            // Use the full delete_track path so audit entries are written
            // and files are cleaned up from disk.
            self.delete_track(caller, track.id).await?;
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
        // Remove the on-disk file before the DB row (best-effort).
        let before_ref = before.as_ref().unwrap();
        if let Some(root) = &self.library_root {
            let candidate = std::path::PathBuf::from(&before_ref.file_path);
            let resolved = if candidate.is_absolute() {
                candidate
            } else {
                root.join(&candidate)
            };
            if let Err(e) = std::fs::remove_file(&resolved) {
                warn!(
                    path = %resolved.display(),
                    error = %e,
                    "delete_track: failed to remove file from disk"
                );
            }
            // Prune now-empty parent dirs (Album, then Artist, then
            // Language) up to the library root. `remove_dir` only succeeds
            // on an empty directory, so this naturally removes the album
            // folder when its last track goes, the artist folder when its
            // last album goes, and the language folder when its last artist
            // goes — while leaving any folder that still holds other files
            // (e.g. cover art) untouched.
            if let Some(parent) = resolved.parent() {
                self.prune_empty_dirs(parent);
            }
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
    // On-disk cleanup
    // -----------------------------------------------------------------------

    /// Remove `dir` and its ancestors as long as each is empty and stays
    /// strictly inside `library_root` (the root itself is never removed).
    ///
    /// `std::fs::remove_dir` fails on a non-empty directory, so this walks
    /// upward and stops at the first directory that still contains entries
    /// (other tracks, other albums, cover art, etc.). Best-effort: any error
    /// other than "not empty" is logged and aborts the walk.
    fn prune_empty_dirs(&self, start: &std::path::Path) {
        let Some(root) = &self.library_root else {
            return;
        };
        // Canonicalise the root once so the boundary check is robust against
        // `.`/`..`/symlink differences.
        let root_canon = match root.canonicalize() {
            Ok(r) => r,
            Err(_) => return,
        };

        let mut dir = start.to_path_buf();
        // Loop while the current dir canonicalises (exists + accessible);
        // every other exit condition `break`s out of the body.
        while let Ok(dir_canon) = dir.canonicalize() {
            // Never touch the root or anything outside it.
            if dir_canon == root_canon || !dir_canon.starts_with(&root_canon) {
                break;
            }

            match std::fs::read_dir(&dir) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        // Not empty — stop here, parents necessarily non-empty too.
                        break;
                    }
                }
                Err(_) => break,
            }

            if let Err(e) = std::fs::remove_dir(&dir) {
                warn!(
                    path = %dir.display(),
                    error = %e,
                    "prune_empty_dirs: failed to remove empty directory"
                );
                break;
            }

            match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            }
        }
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
