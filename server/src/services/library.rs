//! Library management: artists, albums, tracks.
//!
//! Service-layer rules (defense in depth — transport already gated):
//! - Reads: any authed identity.
//! - Mutations: `Manager+`. Every mutation writes an [`audit_log`] row.
//! - Pagination is capped (`MAX_PAGE_LIMIT`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    self as m, Album, AlbumAlias, Artist, ArtistAlias, NewAlbum, NewAlbumAlias, NewArtist,
    NewArtistAlias, NewAuditEntry, NewTrack, NewTrackAlias, PermissionLevel, Track, TrackAlias,
};
use crate::db::repo::{AliasRepo, AlbumRepo, ArtistRepo, AuditRepo, FollowRepo, TrackRepo};
use crate::error::{AppError, Result};
use crate::services::notification::NotificationService;
use crate::services::organizer::sanitize;
use crate::services::tag;

const MAX_PAGE_LIMIT: i64 = 200;
const DEFAULT_PAGE_LIMIT: i64 = 50;

/// Default primary display language when `PRIMARY_LANGUAGE` is unset.
const DEFAULT_PRIMARY_LANGUAGE: &str = "English";

#[derive(Clone)]
pub struct LibraryService {
    pub artists: Arc<dyn ArtistRepo>,
    pub albums: Arc<dyn AlbumRepo>,
    pub tracks: Arc<dyn TrackRepo>,
    pub audit: Arc<dyn AuditRepo>,
    /// Alias store: every known spelling of an artist/album (merge-preserving).
    pub aliases: Arc<dyn AliasRepo>,
    /// Follows: re-pointed onto the survivor when merging artists.
    pub follows: Arc<dyn FollowRepo>,
    /// Language whose spelling is shown as the canonical `name`/`title`
    /// (`PRIMARY_LANGUAGE`, normalized; defaults to `"English"`).
    pub primary_language: String,
    /// Root directory for the organised library.  When set, file deletions
    /// resolve relative `file_path` values against this root and remove the
    /// on-disk file.  Absolute `file_path` values are used as-is.
    pub library_root: Option<PathBuf>,
    /// New-release notifications: when set, creating an album fans out a
    /// notification to every follower of its artist (Phase 10). Optional so the
    /// library service stays constructible without it (e.g. unit tests).
    pub notifications: Option<NotificationService>,
}

impl LibraryService {
    pub fn new(
        artists: Arc<dyn ArtistRepo>,
        albums: Arc<dyn AlbumRepo>,
        tracks: Arc<dyn TrackRepo>,
        audit: Arc<dyn AuditRepo>,
        aliases: Arc<dyn AliasRepo>,
        follows: Arc<dyn FollowRepo>,
    ) -> Self {
        Self {
            artists,
            albums,
            tracks,
            audit,
            aliases,
            follows,
            primary_language: DEFAULT_PRIMARY_LANGUAGE.to_string(),
            library_root: None,
            notifications: None,
        }
    }

    /// Set the library root for file-deletion support.
    pub fn with_library_root(mut self, root: Option<PathBuf>) -> Self {
        self.library_root = root;
        self
    }

    /// Wire in the notification service so new albums alert followers of their
    /// artist (Phase 10).
    pub fn with_notifications(mut self, notifications: NotificationService) -> Self {
        self.notifications = Some(notifications);
        self
    }

    /// Set the primary display language (the spelling shown as `name`/`title`).
    /// Empty/whitespace falls back to the default.
    pub fn with_primary_language(mut self, language: impl Into<String>) -> Self {
        let lang = language.into();
        if !lang.trim().is_empty() {
            self.primary_language = lang;
        }
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
        // Seed the primary alias so the alias table is a complete record of
        // every spelling (merges add to it; reads resolve the display name
        // from it). Best-effort: a seed failure must not fail the create.
        self.seed_artist_alias(&artist).await;
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

    /// Insert the artist's current name as its primary alias (idempotent),
    /// inferring the language from the name's script. Best-effort.
    async fn seed_artist_alias(&self, artist: &Artist) {
        let language = Some(tag::infer_language(&artist.name));
        if let Err(e) = self
            .aliases
            .add_artist_alias(NewArtistAlias {
                artist_id: artist.id,
                name: artist.name.clone(),
                sort_name: artist.sort_name.clone(),
                language,
                is_primary: true,
            })
            .await
        {
            warn!(artist_id = %artist.id, error = %e, "failed to seed artist alias");
        }
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
        // Keep the alias set in sync: register the (possibly new) name as the
        // primary spelling. The previous spelling is retained as a non-primary
        // alias, so a rename never loses a known name.
        self.seed_artist_alias(&after).await;
        if let Ok(aliases) = self.aliases.list_artist_aliases(id).await
            && let Some(a) = aliases.iter().find(|a| a.name == after.name)
        {
            let _ = self.aliases.set_primary_artist_alias(id, a.id).await;
        }
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
        self.seed_album_alias(&album).await;
        self.audit(
            caller,
            "album.create",
            "album",
            Some(album.id),
            None::<&m::Album>,
            Some(&album),
        )
        .await?;
        // New-release fan-out: notify every follower of the artist (Phase 10).
        // Best-effort — a notification failure must never fail the album
        // creation. The actor is excluded so an uploader who follows the artist
        // isn't alerted to their own upload. This single hook covers both
        // manual creates and ingest (which routes through `create_album`).
        if let Some(notifications) = &self.notifications
            && let Err(e) = notifications
                .notify_new_release(caller.user_id(), artist_id, &album)
                .await
        {
            warn!(album_id = %album.id, error = %e, "new-release notification fan-out failed");
        }
        Ok(album)
    }

    /// Insert the album's current title as its primary alias (idempotent),
    /// inferring the language from the title's script. Best-effort.
    async fn seed_album_alias(&self, album: &Album) {
        let language = Some(tag::infer_language(&album.title));
        if let Err(e) = self
            .aliases
            .add_album_alias(NewAlbumAlias {
                album_id: album.id,
                title: album.title.clone(),
                language,
                is_primary: true,
            })
            .await
        {
            warn!(album_id = %album.id, error = %e, "failed to seed album alias");
        }
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
        // Register the (possibly new) title as the primary spelling; the prior
        // title is retained as a non-primary alias.
        self.seed_album_alias(&after).await;
        if let Ok(aliases) = self.aliases.list_album_aliases(id).await
            && let Some(a) = aliases.iter().find(|a| a.title == after.title)
        {
            let _ = self.aliases.set_primary_album_alias(id, a.id).await;
        }
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
        self.seed_track_alias(&track).await;
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
        // Register the (possibly new) title as the primary spelling; the prior
        // title is retained as a non-primary alias. Mirrors `update_album`.
        self.seed_track_alias(&after).await;
        if let Ok(aliases) = self.aliases.list_track_aliases(id).await
            && let Some(a) = aliases.iter().find(|a| a.title == after.title)
        {
            let _ = self.aliases.set_primary_track_alias(id, a.id).await;
        }
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
        let album_id = before_ref.album_id;
        self.tracks.delete(id).await?;
        // Deleting a track can drop the album's last explicit song → recompute
        // the rollup (harmless when the album is itself being cascade-deleted).
        self.albums.recompute_explicit(album_id).await?;
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
    // Merge + aliases (artists / albums)
    // -----------------------------------------------------------------------

    /// Merge the `duplicate` artist into `survivor`: re-point the duplicate's
    /// albums, tracks, and followers onto the survivor, preserve every one of
    /// its spellings as a survivor alias, delete the duplicate row, then
    /// re-derive the survivor's display name from the merged alias set.
    /// Destructive + audited (`artist.merge`). Manager+.
    pub async fn merge_artists(
        &self,
        caller: &Identity,
        survivor_id: Uuid,
        duplicate_id: Uuid,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        if survivor_id == duplicate_id {
            return Err(AppError::InvalidArgument(
                "cannot merge an artist into itself".into(),
            ));
        }
        let survivor = self
            .artists
            .get(survivor_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {survivor_id}")))?;
        let duplicate = self
            .artists
            .get(duplicate_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {duplicate_id}")))?;

        // Ensure both current names exist as aliases before moving anything.
        self.seed_artist_alias(&survivor).await;
        self.seed_artist_alias(&duplicate).await;

        // Re-point the duplicate's catalog + followers onto the survivor.
        self.albums.reassign_artist(duplicate_id, survivor_id).await?;
        self.tracks.reassign_artist(duplicate_id, survivor_id).await?;
        self.follows.reassign_artist(duplicate_id, survivor_id).await?;

        // Preserve the duplicate's spellings on the survivor, then delete it
        // (any leftover alias rows cascade with the row).
        self.aliases
            .reassign_artist_aliases(duplicate_id, survivor_id)
            .await?;
        self.artists.delete(duplicate_id).await?;

        // Re-derive the survivor's primary-language display name.
        self.recompute_artist_display(survivor_id).await?;
        let after = self
            .artists
            .get(survivor_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {survivor_id}")))?;
        let before = serde_json::json!({ "survivor": survivor, "duplicate": duplicate });
        self.audit(caller, "artist.merge", "artist", Some(survivor_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Merge the `duplicate` album into `survivor`: re-point its tracks,
    /// preserve its title spellings, delete it, re-derive the display title.
    /// Destructive + audited (`album.merge`). Manager+.
    pub async fn merge_albums(
        &self,
        caller: &Identity,
        survivor_id: Uuid,
        duplicate_id: Uuid,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        if survivor_id == duplicate_id {
            return Err(AppError::InvalidArgument(
                "cannot merge an album into itself".into(),
            ));
        }
        let survivor = self
            .albums
            .get(survivor_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {survivor_id}")))?;
        let duplicate = self
            .albums
            .get(duplicate_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {duplicate_id}")))?;

        self.seed_album_alias(&survivor).await;
        self.seed_album_alias(&duplicate).await;

        self.tracks.reassign_album(duplicate_id, survivor_id).await?;
        self.aliases
            .reassign_album_aliases(duplicate_id, survivor_id)
            .await?;
        self.albums.delete(duplicate_id).await?;

        // The duplicate's tracks now belong to the survivor → its explicit
        // rollup may have gained an explicit song.
        self.albums.recompute_explicit(survivor_id).await?;
        self.recompute_album_display(survivor_id).await?;
        let after = self
            .albums
            .get(survivor_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {survivor_id}")))?;
        let before = serde_json::json!({ "survivor": survivor, "duplicate": duplicate });
        self.audit(caller, "album.merge", "album", Some(survivor_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Move a track into `target_album_id`, optionally flagging it a single
    /// release within that album. When the source album is left empty (the
    /// usual case for a one-track "single" album), it is pruned. Audited
    /// (`track.move`). Manager+.
    pub async fn move_track(
        &self,
        caller: &Identity,
        track_id: Uuid,
        target_album_id: Uuid,
        single_release: bool,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        if self.albums.get(target_album_id).await?.is_none() {
            return Err(AppError::NotFound(format!("album {target_album_id}")));
        }
        let source_album_id = before.album_id;
        self.tracks.set_album(track_id, target_album_id).await?;
        let after = self
            .tracks
            .set_single_release(track_id, single_release)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.move", "track", Some(track_id), Some(&before), Some(&after))
            .await?;

        // The moved track carries its explicit flag across, so both albums'
        // rollups may change (source may lose its only explicit track; target
        // may gain one).
        self.albums.recompute_explicit(target_album_id).await?;
        if source_album_id != target_album_id {
            self.albums.recompute_explicit(source_album_id).await?;
        }

        // Prune a now-empty source album (best-effort; the leftover "single").
        if source_album_id != target_album_id
            && self.tracks.list_by_album(source_album_id).await?.is_empty()
            && let Err(e) = self.delete_album(caller, source_album_id).await
        {
            warn!(album_id = %source_album_id, error = %e, "move_track: failed to prune empty source album");
        }
        Ok(after)
    }

    /// Toggle a track's single-release flag (no album change). Audited. Manager+.
    ///
    /// Guards the album-level invariant: a `single`-type album must keep at
    /// least one single track, so clearing its last one is rejected.
    pub async fn set_track_single_release(
        &self,
        caller: &Identity,
        track_id: Uuid,
        single_release: bool,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        if !single_release
            && before.is_single_release
            && let Some(album) = self.albums.get(before.album_id).await?
            && album.album_type == "single"
        {
            let others_single = self
                .tracks
                .list_by_album(before.album_id)
                .await?
                .iter()
                .filter(|t| t.id != track_id && t.is_single_release)
                .count();
            if others_single == 0 {
                return Err(AppError::InvalidArgument(
                    "cannot clear the last single of a single-type album; change the \
                     album type first or flag another track"
                        .to_string(),
                ));
            }
        }
        let after = self
            .tracks
            .set_single_release(track_id, single_release)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.update", "track", Some(track_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Toggle a track's explicit flag (independent of the title). Audited.
    /// Manager+. Recomputes the album's `is_explicit` rollup so a song becoming
    /// (or no longer being) explicit flips the album label accordingly.
    pub async fn set_track_explicit(
        &self,
        caller: &Identity,
        track_id: Uuid,
        explicit: bool,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        let after = self
            .tracks
            .set_explicit(track_id, explicit)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.albums.recompute_explicit(after.album_id).await?;
        self.audit(caller, "track.update", "track", Some(track_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Set an album's classification (`album` / `ep` / `single`). Manager+,
    /// audited (`album.set_type`).
    ///
    /// A `single` album must have at least one track flagged `is_single_release`.
    /// When `single_track_id` is given (and the target type is `single`) that
    /// track is flagged first — it must belong to the album — and the invariant
    /// is then verified against the album's tracks, else `InvalidArgument`.
    pub async fn set_album_type(
        &self,
        caller: &Identity,
        album_id: Uuid,
        album_type: &str,
        single_track_id: Option<Uuid>,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        let album_type = parse_album_type(album_type)?;
        let before = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;

        // Optionally flag the caller-chosen main single first (single albums).
        if album_type == "single"
            && let Some(track_id) = single_track_id
        {
            let track = self
                .tracks
                .get(track_id)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
            if track.album_id != album_id {
                return Err(AppError::InvalidArgument(format!(
                    "track {track_id} is not on album {album_id}"
                )));
            }
            if !track.is_single_release {
                self.tracks.set_single_release(track_id, true).await?;
            }
        }

        // Enforce the single-song invariant before persisting the type.
        if album_type == "single" {
            let single_count = self
                .tracks
                .list_by_album(album_id)
                .await?
                .iter()
                .filter(|t| t.is_single_release)
                .count();
            if !single_song_rule_satisfied(&album_type, single_count) {
                return Err(AppError::InvalidArgument(
                    "a single album needs at least one track marked as its single song"
                        .to_string(),
                ));
            }
        }

        let after = self
            .albums
            .set_album_type(album_id, &album_type)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        self.audit(caller, "album.set_type", "album", Some(album_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    // ----- Artist aliases -----

    pub async fn list_artist_aliases(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<Vec<ArtistAlias>> {
        caller.require(PermissionLevel::User)?;
        self.aliases.list_artist_aliases(artist_id).await
    }

    /// Add an alternate spelling to an artist. The display name re-resolves
    /// (a new primary-language spelling becomes canonical). Audited. Manager+.
    pub async fn add_artist_alias(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        if name.trim().is_empty() {
            return Err(AppError::InvalidArgument("alias name is required".into()));
        }
        let before = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        self.aliases
            .add_artist_alias(NewArtistAlias {
                artist_id,
                name: name.to_string(),
                sort_name: sort_name.map(str::to_string),
                language: Some(resolve_language(language, name)),
                is_primary: false,
            })
            .await?;
        self.recompute_artist_display(artist_id).await?;
        let after = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        self.audit(caller, "artist.alias.add", "artist", Some(artist_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Remove an alias (refuses the last remaining spelling). Re-resolves the
    /// display name afterwards. Audited. Manager+.
    pub async fn remove_artist_alias(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        let alias = self
            .aliases
            .get_artist_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.artist_id != artist_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this artist".into(),
            ));
        }
        if self.aliases.list_artist_aliases(artist_id).await?.len() <= 1 {
            return Err(AppError::InvalidArgument(
                "cannot remove the only spelling of an artist".into(),
            ));
        }
        self.aliases.delete_artist_alias(alias_id).await?;
        self.recompute_artist_display(artist_id).await?;
        let after = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        self.audit(caller, "artist.alias.remove", "artist", Some(artist_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Force a specific alias to be the displayed spelling (a manual override
    /// of the language pick — sticks until the next merge/add re-resolves and
    /// finds a primary-language match). Audited. Manager+.
    pub async fn set_primary_artist_alias(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Artist> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        let alias = self
            .aliases
            .get_artist_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.artist_id != artist_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this artist".into(),
            ));
        }
        self.aliases.set_primary_artist_alias(artist_id, alias_id).await?;
        self.artists.update(artist_id, &alias.name, alias.sort_name.as_deref()).await?;
        let after = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        self.audit(caller, "artist.update", "artist", Some(artist_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    // ----- Album aliases -----

    pub async fn list_album_aliases(
        &self,
        caller: &Identity,
        album_id: Uuid,
    ) -> Result<Vec<AlbumAlias>> {
        caller.require(PermissionLevel::User)?;
        self.aliases.list_album_aliases(album_id).await
    }

    pub async fn add_album_alias(
        &self,
        caller: &Identity,
        album_id: Uuid,
        title: &str,
        language: Option<&str>,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        if title.trim().is_empty() {
            return Err(AppError::InvalidArgument("alias title is required".into()));
        }
        let before = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        self.aliases
            .add_album_alias(NewAlbumAlias {
                album_id,
                title: title.to_string(),
                language: Some(resolve_language(language, title)),
                is_primary: false,
            })
            .await?;
        self.recompute_album_display(album_id).await?;
        let after = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        self.audit(caller, "album.alias.add", "album", Some(album_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn remove_album_alias(
        &self,
        caller: &Identity,
        album_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        let alias = self
            .aliases
            .get_album_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.album_id != album_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this album".into(),
            ));
        }
        if self.aliases.list_album_aliases(album_id).await?.len() <= 1 {
            return Err(AppError::InvalidArgument(
                "cannot remove the only spelling of an album".into(),
            ));
        }
        self.aliases.delete_album_alias(alias_id).await?;
        self.recompute_album_display(album_id).await?;
        let after = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        self.audit(caller, "album.alias.remove", "album", Some(album_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn set_primary_album_alias(
        &self,
        caller: &Identity,
        album_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Album> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        let alias = self
            .aliases
            .get_album_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.album_id != album_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this album".into(),
            ));
        }
        self.aliases.set_primary_album_alias(album_id, alias_id).await?;
        self.albums
            .update(album_id, &alias.title, before.release_year, before.cover_path.as_deref())
            .await?;
        let after = self
            .albums
            .get(album_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
        self.audit(caller, "album.update", "album", Some(album_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    // ----- Track aliases -----

    pub async fn list_track_aliases(
        &self,
        caller: &Identity,
        track_id: Uuid,
    ) -> Result<Vec<TrackAlias>> {
        caller.require(PermissionLevel::User)?;
        self.aliases.list_track_aliases(track_id).await
    }

    pub async fn add_track_alias(
        &self,
        caller: &Identity,
        track_id: Uuid,
        title: &str,
        language: Option<&str>,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        if title.trim().is_empty() {
            return Err(AppError::InvalidArgument("alias title is required".into()));
        }
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.aliases
            .add_track_alias(NewTrackAlias {
                track_id,
                title: title.to_string(),
                language: Some(resolve_language(language, title)),
                is_primary: false,
            })
            .await?;
        self.recompute_track_display(track_id).await?;
        let after = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.alias.add", "track", Some(track_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn remove_track_alias(
        &self,
        caller: &Identity,
        track_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        let alias = self
            .aliases
            .get_track_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.track_id != track_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this track".into(),
            ));
        }
        if self.aliases.list_track_aliases(track_id).await?.len() <= 1 {
            return Err(AppError::InvalidArgument(
                "cannot remove the only spelling of a track".into(),
            ));
        }
        self.aliases.delete_track_alias(alias_id).await?;
        self.recompute_track_display(track_id).await?;
        let after = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.alias.remove", "track", Some(track_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    pub async fn set_primary_track_alias(
        &self,
        caller: &Identity,
        track_id: Uuid,
        alias_id: Uuid,
    ) -> Result<Track> {
        caller.require(PermissionLevel::Manager)?;
        let before = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        let alias = self
            .aliases
            .get_track_alias(alias_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("alias {alias_id}")))?;
        if alias.track_id != track_id {
            return Err(AppError::InvalidArgument(
                "alias does not belong to this track".into(),
            ));
        }
        self.aliases.set_primary_track_alias(track_id, alias_id).await?;
        self.tracks
            .update(track_id, &alias.title, before.track_no, before.disc_no, &before.metadata_json)
            .await?;
        let after = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.update", "track", Some(track_id), Some(&before), Some(&after))
            .await?;
        Ok(after)
    }

    /// Seed a track's current title as its primary alias (idempotent upsert).
    /// Called on create/update so the alias table is always complete.
    async fn seed_track_alias(&self, track: &Track) {
        let language = Some(tag::infer_language(&track.title));
        if let Err(e) = self
            .aliases
            .add_track_alias(NewTrackAlias {
                track_id: track.id,
                title: track.title.clone(),
                language,
                is_primary: true,
            })
            .await
        {
            warn!(track_id = %track.id, error = %e, "failed to seed track alias");
        }
    }

    // ----- Display-name resolution -----

    /// Re-point `artists.name`/`sort_name` at the spelling whose language
    /// matches `primary_language`. When nothing matches, the current primary
    /// (or first) alias is kept so a manual choice is never clobbered.
    async fn recompute_artist_display(&self, id: Uuid) -> Result<()> {
        let aliases = self.aliases.list_artist_aliases(id).await?;
        if aliases.is_empty() {
            return Ok(());
        }
        let candidates: Vec<(String, Option<String>)> = aliases
            .iter()
            .map(|a| (a.name.clone(), a.language.clone()))
            .collect();
        let chosen = match pick_primary_index(&candidates, &self.primary_language) {
            Some(i) => &aliases[i],
            None => aliases.iter().find(|a| a.is_primary).unwrap_or(&aliases[0]),
        };
        self.aliases.set_primary_artist_alias(id, chosen.id).await?;
        if let Some(cur) = self.artists.get(id).await?
            && (cur.name != chosen.name || cur.sort_name != chosen.sort_name)
        {
            self.artists.update(id, &chosen.name, chosen.sort_name.as_deref()).await?;
        }
        Ok(())
    }

    async fn recompute_album_display(&self, id: Uuid) -> Result<()> {
        let aliases = self.aliases.list_album_aliases(id).await?;
        if aliases.is_empty() {
            return Ok(());
        }
        let candidates: Vec<(String, Option<String>)> = aliases
            .iter()
            .map(|a| (a.title.clone(), a.language.clone()))
            .collect();
        let chosen = match pick_primary_index(&candidates, &self.primary_language) {
            Some(i) => &aliases[i],
            None => aliases.iter().find(|a| a.is_primary).unwrap_or(&aliases[0]),
        };
        self.aliases.set_primary_album_alias(id, chosen.id).await?;
        if let Some(cur) = self.albums.get(id).await?
            && cur.title != chosen.title
        {
            self.albums
                .update(id, &chosen.title, cur.release_year, cur.cover_path.as_deref())
                .await?;
        }
        Ok(())
    }

    /// Re-point `tracks.title` at the spelling whose language matches
    /// `primary_language`. When nothing matches, the current primary (or first)
    /// alias is kept so a manual choice is never clobbered. Mirrors
    /// [`recompute_album_display`].
    async fn recompute_track_display(&self, id: Uuid) -> Result<()> {
        let aliases = self.aliases.list_track_aliases(id).await?;
        if aliases.is_empty() {
            return Ok(());
        }
        let candidates: Vec<(String, Option<String>)> = aliases
            .iter()
            .map(|a| (a.title.clone(), a.language.clone()))
            .collect();
        let chosen = match pick_primary_index(&candidates, &self.primary_language) {
            Some(i) => &aliases[i],
            None => aliases.iter().find(|a| a.is_primary).unwrap_or(&aliases[0]),
        };
        self.aliases.set_primary_track_alias(id, chosen.id).await?;
        if let Some(cur) = self.tracks.get(id).await?
            && cur.title != chosen.title
        {
            self.tracks
                .update(id, &chosen.title, cur.track_no, cur.disc_no, &cur.metadata_json)
                .await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Library storage location (per-artist language folder)
    // -----------------------------------------------------------------------

    /// List the distinct on-disk `<Language>/<Artist>` directories an artist's
    /// tracks currently live under, plus the language folders present in the
    /// library (so the UI can offer "known + existing" targets).
    ///
    /// An artist ends up with more than one entry when its tracks were ingested
    /// under different language tags / name spellings (e.g. `English/aespa` and
    /// `Korean/에스파`). User+ (read).
    pub async fn list_artist_library_paths(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<ArtistStoragePaths> {
        caller.require(PermissionLevel::User)?;
        // Validate the artist exists for a clean 404.
        if self.artists.get(artist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("artist {artist_id}")));
        }
        let Some(root) = &self.library_root else {
            // No library root configured — nothing to enumerate.
            return Ok(ArtistStoragePaths::default());
        };

        let tracks = self.artist_tracks(artist_id).await?;
        let mut paths: Vec<ArtistLibraryPath> = Vec::new();
        for t in &tracks {
            let Some(rel) = relative_to_root(&t.file_path, root) else {
                continue;
            };
            let Some((language, artist_folder)) = lang_artist_of(&rel) else {
                continue;
            };
            let relative_dir = format!("{language}/{artist_folder}");
            let bytes = t.file_size.unwrap_or(0);
            if let Some(g) = paths.iter_mut().find(|g| g.relative_dir == relative_dir) {
                g.track_count += 1;
                g.storage_bytes += bytes;
            } else {
                paths.push(ArtistLibraryPath {
                    language,
                    artist_folder,
                    relative_dir,
                    track_count: 1,
                    storage_bytes: bytes,
                });
            }
        }
        paths.sort_by(|a, b| a.relative_dir.cmp(&b.relative_dir));

        Ok(ArtistStoragePaths {
            paths,
            library_languages: top_level_dirs(root),
        })
    }

    /// Move **all** of an artist's tracks so they live under a single
    /// `<target_language>/<target_folder>` directory, physically relocating the
    /// files, updating each `file_path`, carrying album covers/sidecars along,
    /// and pruning the emptied source folders.
    ///
    /// `target_folder` (the on-disk artist-folder spelling) is resolved when
    /// omitted: an existing folder already in `target_language` wins (merge into
    /// it), else an alias declared in that language, else the artist's most
    /// common current folder. Storage-only — the display name/aliases are left
    /// untouched. Manager+, audited (`artist.relocate`).
    pub async fn set_artist_language(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        target_language: &str,
        target_folder: Option<&str>,
    ) -> Result<RelocateReport> {
        caller.require(PermissionLevel::Manager)?;
        if target_language.trim().is_empty() {
            return Err(AppError::InvalidArgument("target_language is required".into()));
        }
        let root = self
            .library_root
            .clone()
            .ok_or_else(|| AppError::Config("LIBRARY_PATH is not set".into()))?;
        let artist = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;

        let target_lang = sanitize(target_language);
        let albums = self.albums.list_by_artist(artist_id).await?;
        let mut tracks = Vec::new();
        for al in &albums {
            tracks.extend(self.tracks.list_by_album(al.id).await?);
        }

        // Resolve the destination artist-folder spelling.
        let target_folder = match target_folder {
            Some(f) if !f.trim().is_empty() => sanitize(f),
            _ => self
                .resolve_target_folder(&tracks, &root, &target_lang, &artist)
                .await,
        };
        let target_relative_dir = format!("{target_lang}/{target_folder}");

        // 1. Move every track file into the target dir, updating file_path.
        let mut report = RelocateReport {
            target_relative_dir: target_relative_dir.clone(),
            ..Default::default()
        };
        let mut source_album_dirs: Vec<PathBuf> = Vec::new();
        let mut source_prefixes: Vec<String> = Vec::new();
        for t in &tracks {
            let Some(rel) = relative_to_root(&t.file_path, &root) else {
                report.skipped += 1;
                continue;
            };
            if let Some((lang, folder)) = lang_artist_of(&rel) {
                let prefix = format!("{lang}/{folder}");
                if !source_prefixes.contains(&prefix) {
                    source_prefixes.push(prefix);
                }
            }
            let Some(new_rel) = retarget(&rel, &target_lang, &target_folder) else {
                report.skipped += 1;
                continue;
            };
            let src_abs = resolve_abs(&t.file_path, &root);
            let dst_abs = root.join(&new_rel);
            if dst_abs == src_abs {
                report.skipped += 1;
                continue;
            }
            match move_file(&src_abs, &dst_abs) {
                Ok(MoveOutcome::Moved) | Ok(MoveOutcome::AlreadyPresent) => {
                    self.tracks
                        .update_file_path(t.id, &dst_abs.to_string_lossy())
                        .await?;
                    report.moved += 1;
                    if let Some(parent) = src_abs.parent() {
                        let parent = parent.to_path_buf();
                        if !source_album_dirs.contains(&parent) {
                            source_album_dirs.push(parent);
                        }
                    }
                }
                Ok(MoveOutcome::Conflict) => {
                    warn!(
                        track = %t.id,
                        src = %src_abs.display(),
                        dst = %dst_abs.display(),
                        "relocate: destination exists with different content, skipping"
                    );
                    report.skipped += 1;
                }
                Ok(MoveOutcome::Missing) => {
                    report.skipped += 1;
                }
                Err(e) => {
                    warn!(track = %t.id, error = %e, "relocate: file move failed");
                    report.skipped += 1;
                }
            }
        }

        // 2. Sweep any leftover files (cover.jpg, artwork, logs) from each
        //    source album dir into the matching destination album dir so
        //    nothing is orphaned before pruning.
        for src_dir in &source_album_dirs {
            let Some(basename) = src_dir.file_name() else {
                continue;
            };
            let dst_dir = root.join(&target_lang).join(&target_folder).join(basename);
            if dst_dir == *src_dir {
                continue;
            }
            move_leftover_files(src_dir, &dst_dir);
        }

        // 3. Re-point album covers that lived under an old prefix at their new
        //    on-disk location (only when the moved file actually exists).
        for al in &albums {
            let Some(cover) = &al.cover_path else { continue };
            let Some(rel) = relative_to_root(cover, &root) else {
                continue;
            };
            let Some(new_rel) = retarget(&rel, &target_lang, &target_folder) else {
                continue;
            };
            let new_abs = root.join(&new_rel);
            if new_abs == resolve_abs(cover, &root) || !new_abs.is_file() {
                continue;
            }
            if let Err(e) = self
                .albums
                .update(al.id, &al.title, al.release_year, Some(&new_abs.to_string_lossy()))
                .await
            {
                warn!(album = %al.id, error = %e, "relocate: cover_path update failed");
            }
        }

        // 4. Prune the now-empty source folders (album → artist → language),
        //    deleting e.g. `English/aespa` so no dangling folder remains.
        for src_dir in &source_album_dirs {
            self.prune_empty_dirs(src_dir);
        }

        // Audit the move (before = source dirs, after = target dir).
        let before = serde_json::json!({ "sources": source_prefixes });
        let after = serde_json::json!({
            "target": target_relative_dir,
            "moved": report.moved,
            "skipped": report.skipped,
        });
        self.audit(caller, "artist.relocate", "artist", Some(artist_id), Some(&before), Some(&after))
            .await?;

        // File sizes are unchanged by a move, so storage aggregates need no
        // recompute.
        Ok(report)
    }

    /// Every track of an artist, gathered across all their albums (reuses the
    /// existing per-album listing so no new repo method is needed).
    async fn artist_tracks(&self, artist_id: Uuid) -> Result<Vec<Track>> {
        let albums = self.albums.list_by_artist(artist_id).await?;
        let mut tracks = Vec::new();
        for al in &albums {
            tracks.extend(self.tracks.list_by_album(al.id).await?);
        }
        Ok(tracks)
    }

    /// Resolve the destination artist-folder spelling for a relocation when the
    /// caller didn't pin one. Precedence: an existing folder already in the
    /// target language → an alias declared in that language → the most common
    /// current folder → the artist's display name.
    async fn resolve_target_folder(
        &self,
        tracks: &[Track],
        root: &Path,
        target_lang: &str,
        artist: &Artist,
    ) -> String {
        // 1. Existing folder already under the target language.
        let mut folder_counts: Vec<(String, usize)> = Vec::new();
        for t in tracks {
            let Some(rel) = relative_to_root(&t.file_path, root) else {
                continue;
            };
            let Some((lang, folder)) = lang_artist_of(&rel) else {
                continue;
            };
            if lang.eq_ignore_ascii_case(target_lang) {
                return folder;
            }
            if let Some(e) = folder_counts.iter_mut().find(|(k, _)| *k == folder) {
                e.1 += 1;
            } else {
                folder_counts.push((folder, 1));
            }
        }
        // 2. An alias declared in (or inferred to be) the target language.
        let target_norm = tag::normalize_language(target_lang);
        if let Ok(aliases) = self.aliases.list_artist_aliases(artist.id).await {
            if let Some(a) = aliases.iter().find(|a| {
                let lang = match &a.language {
                    Some(s) if !s.trim().is_empty() => tag::normalize_language(s),
                    _ => tag::infer_language(&a.name),
                };
                lang == target_norm
            }) {
                return sanitize(&a.name);
            }
        }
        // 3. Most common current folder, else the display name.
        folder_counts
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(f, _)| f)
            .unwrap_or_else(|| sanitize(&artist.name))
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

/// The valid album classifications.
const ALBUM_TYPES: [&str; 4] = ["album", "ep", "single", "live"];

/// Normalize + validate an album-type string, returning the canonical lowercase
/// value or an `InvalidArgument` listing the allowed values.
fn parse_album_type(raw: &str) -> Result<String> {
    let t = raw.trim().to_ascii_lowercase();
    if ALBUM_TYPES.contains(&t.as_str()) {
        Ok(t)
    } else {
        Err(AppError::InvalidArgument(format!(
            "album_type must be one of album, ep, single, live (got {raw:?})"
        )))
    }
}

/// The single-song invariant: only a `single` album requires at least one of
/// its tracks to be flagged `is_single_release`. `album`/`ep`/`live` are
/// unrestricted.
fn single_song_rule_satisfied(album_type: &str, single_track_count: usize) -> bool {
    album_type != "single" || single_track_count >= 1
}

/// Resolve the language label for an alias: the explicit value when given
/// (normalized via the shared tag normalizer), else inferred from the
/// spelling's script.
fn resolve_language(explicit: Option<&str>, label: &str) -> String {
    match explicit {
        Some(s) if !s.trim().is_empty() => tag::normalize_language(s),
        _ => tag::infer_language(label),
    }
}

/// Choose the index of the spelling whose language matches `primary_language`.
///
/// Each candidate is `(label, stored_language)`; when the stored language is
/// absent it is inferred from the label's script (so the SQL backfill can leave
/// `language` NULL). Returns the first match in the given order (the repo lists
/// the current primary first, then oldest), or `None` when nothing matches —
/// in which case the caller keeps the current display name.
fn pick_primary_index(
    candidates: &[(String, Option<String>)],
    primary_language: &str,
) -> Option<usize> {
    let target = tag::normalize_language(primary_language);
    candidates.iter().position(|(label, lang)| {
        let resolved = match lang {
            Some(s) if !s.trim().is_empty() => tag::normalize_language(s),
            _ => tag::infer_language(label),
        };
        resolved == target
    })
}

// ---------------------------------------------------------------------------
// Artist library-storage DTOs + path helpers
// ---------------------------------------------------------------------------

/// One distinct `<Language>/<Artist>` directory an artist's tracks live under.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistLibraryPath {
    /// Top-level language folder (first path component), e.g. `"English"`.
    pub language: String,
    /// Artist folder spelling (second path component), e.g. `"aespa"`.
    pub artist_folder: String,
    /// `"<language>/<artist_folder>"` — the group key shown to the user.
    pub relative_dir: String,
    /// Number of the artist's tracks under this directory.
    pub track_count: u64,
    /// Sum of those tracks' on-disk bytes.
    pub storage_bytes: i64,
}

/// Result of [`LibraryService::list_artist_library_paths`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct ArtistStoragePaths {
    /// Distinct directories the artist currently occupies (a length > 1 is the
    /// "split across languages" warning the UI surfaces).
    pub paths: Vec<ArtistLibraryPath>,
    /// Language folders that already exist at the top of the library, so the UI
    /// can offer them alongside a known-language list.
    pub library_languages: Vec<String>,
}

/// Result of [`LibraryService::set_artist_language`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct RelocateReport {
    /// Tracks whose file was moved (or already present at the target).
    pub moved: u64,
    /// Tracks skipped (already at target, unresolvable path, or a conflict).
    pub skipped: u64,
    /// `"<language>/<artist_folder>"` the artist now lives under.
    pub target_relative_dir: String,
}

/// Outcome of a single file move (see [`move_file`]).
enum MoveOutcome {
    /// The file was physically moved.
    Moved,
    /// An identical file already existed at the destination (source removed).
    AlreadyPresent,
    /// A *different* file exists at the destination — left untouched.
    Conflict,
    /// Neither source nor destination exists.
    Missing,
}

/// Strip `root` from a stored `file_path`, yielding the library-relative path.
/// Handles both absolute (the ingest default) and already-relative values.
fn relative_to_root(file_path: &str, root: &Path) -> Option<PathBuf> {
    let p = Path::new(file_path);
    if p.is_absolute() {
        p.strip_prefix(root).ok().map(Path::to_path_buf)
    } else {
        Some(p.to_path_buf())
    }
}

/// Resolve a stored `file_path` to an absolute path against `root`.
fn resolve_abs(file_path: &str, root: &Path) -> PathBuf {
    let p = Path::new(file_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// Extract `(language, artist_folder)` from a library-relative track path.
/// Requires at least three components (`<lang>/<artist>/<album…>/<file>`).
fn lang_artist_of(rel: &Path) -> Option<(String, String)> {
    let mut it = rel.components();
    let language = it.next()?.as_os_str().to_str()?.to_string();
    let artist = it.next()?.as_os_str().to_str()?.to_string();
    it.next()?; // ensure there's a track/album component beyond the artist dir
    Some((language, artist))
}

/// Rewrite a library-relative path so its first two components become
/// `<target_language>/<target_folder>`, preserving the album+file suffix.
/// Returns `None` when the path has fewer than three components.
fn retarget(rel: &Path, target_language: &str, target_folder: &str) -> Option<PathBuf> {
    let comps: Vec<_> = rel.components().collect();
    if comps.len() < 3 {
        return None;
    }
    let mut out = PathBuf::from(target_language);
    out.push(target_folder);
    for c in &comps[2..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// Immediate sub-directory names of `root` (the existing language folders).
fn top_level_dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
                && !name.starts_with('.')
            {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Move `src` to `dst`, creating parent dirs and falling back to copy+remove
/// across filesystems. Never overwrites a differing destination file.
fn move_file(src: &Path, dst: &Path) -> std::io::Result<MoveOutcome> {
    let src_exists = src.exists();
    let dst_exists = dst.exists();
    if !src_exists {
        return Ok(if dst_exists {
            MoveOutcome::AlreadyPresent
        } else {
            MoveOutcome::Missing
        });
    }
    if dst_exists {
        let s = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        let d = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(u64::MAX);
        if s == d {
            // Identical file already there — drop the redundant source copy.
            std::fs::remove_file(src)?;
            return Ok(MoveOutcome::AlreadyPresent);
        }
        return Ok(MoveOutcome::Conflict);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(MoveOutcome::Moved),
        Err(_) => {
            // Cross-device rename fails on Windows/Unix alike — copy then remove.
            std::fs::copy(src, dst)?;
            std::fs::remove_file(src)?;
            Ok(MoveOutcome::Moved)
        }
    }
}

/// Move every plain file remaining in `src_dir` into `dst_dir` (covers, stray
/// artwork, logs) so the album folder can be pruned cleanly. Best-effort.
fn move_leftover_files(src_dir: &Path, dst_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(src_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else { continue };
        let dst = dst_dir.join(name);
        if let Err(e) = move_file(&path, &dst) {
            warn!(
                src = %path.display(),
                dst = %dst.display(),
                error = %e,
                "relocate: leftover file move failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lang_artist_of, parse_album_type, pick_primary_index, relative_to_root, resolve_language,
        retarget, single_song_rule_satisfied,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn relative_to_root_handles_absolute_and_relative() {
        // Use temp_dir() as the base so the "absolute" arm is genuinely
        // absolute on both Windows (drive-letter) and Unix.
        let root = std::env::temp_dir().join("octave-relroot-test");
        let abs = root.join("English/aespa/Album/01.flac");
        assert_eq!(
            relative_to_root(&abs.to_string_lossy(), &root),
            Some(PathBuf::from("English/aespa/Album/01.flac"))
        );
        // Already relative → returned as-is.
        assert_eq!(
            relative_to_root("Korean/에스파/Album/01.flac", &root),
            Some(PathBuf::from("Korean/에스파/Album/01.flac"))
        );
        // Absolute but outside the root → None (can't reorganize).
        let outside = std::env::temp_dir().join("octave-other/x/y/z.flac");
        assert_eq!(relative_to_root(&outside.to_string_lossy(), &root), None);
    }

    #[test]
    fn lang_artist_of_needs_three_components() {
        assert_eq!(
            lang_artist_of(Path::new("English/aespa/Album/01.flac")),
            Some(("English".into(), "aespa".into()))
        );
        // lang/artist/file (no album) still counts — three components.
        assert_eq!(
            lang_artist_of(Path::new("Korean/에스파/track.flac")),
            Some(("Korean".into(), "에스파".into()))
        );
        // Only two components → not a real track path.
        assert_eq!(lang_artist_of(Path::new("English/aespa")), None);
    }

    #[test]
    fn retarget_replaces_language_and_artist_keeps_suffix() {
        let rel = Path::new("English/aespa/Armageddon/01 - Supernova.flac");
        assert_eq!(
            retarget(rel, "Korean", "에스파"),
            Some(PathBuf::from("Korean/에스파/Armageddon/01 - Supernova.flac"))
        );
        // Fewer than three components → nothing to retarget.
        assert_eq!(retarget(Path::new("English/aespa"), "Korean", "에스파"), None);
    }


    fn cand(name: &str, lang: Option<&str>) -> (String, Option<String>) {
        (name.to_string(), lang.map(str::to_string))
    }

    #[test]
    fn picks_english_spelling_for_english_primary() {
        // YUQI (English) + 우기 ((여자)아이들) (Korean), languages stored.
        let candidates = vec![
            cand("우기 ((여자)아이들)", Some("Korean")),
            cand("YUQI", Some("English")),
        ];
        assert_eq!(pick_primary_index(&candidates, "English"), Some(1));
        assert_eq!(pick_primary_index(&candidates, "en"), Some(1));
    }

    #[test]
    fn infers_language_when_unstored() {
        // No stored language → inferred from the script of each spelling.
        let candidates = vec![cand("우기 ((여자)아이들)", None), cand("YUQI", None)];
        assert_eq!(pick_primary_index(&candidates, "English"), Some(1));
        assert_eq!(pick_primary_index(&candidates, "Korean"), Some(0));
    }

    #[test]
    fn no_match_returns_none() {
        // Only a Korean spelling, primary language English → no match (caller
        // keeps the current display name).
        let candidates = vec![cand("우기 ((여자)아이들)", Some("Korean"))];
        assert_eq!(pick_primary_index(&candidates, "English"), None);
    }

    #[test]
    fn first_match_wins() {
        let candidates = vec![
            cand("YUQI", Some("English")),
            cand("Yuqi", Some("English")),
        ];
        assert_eq!(pick_primary_index(&candidates, "English"), Some(0));
    }

    #[test]
    fn resolve_language_prefers_explicit_then_infers() {
        assert_eq!(resolve_language(Some("ko"), "YUQI"), "Korean");
        assert_eq!(resolve_language(Some("  "), "우기"), "Korean"); // blank → infer
        assert_eq!(resolve_language(None, "YUQI"), "English");
    }

    #[test]
    fn parse_album_type_normalizes_and_validates() {
        assert_eq!(parse_album_type("album").unwrap(), "album");
        assert_eq!(parse_album_type("EP").unwrap(), "ep"); // case-insensitive
        assert_eq!(parse_album_type("  Single ").unwrap(), "single"); // trimmed
        assert_eq!(parse_album_type("Live").unwrap(), "live");
        assert!(parse_album_type("mixtape").is_err());
        assert!(parse_album_type("").is_err());
    }

    #[test]
    fn single_song_rule_only_binds_single_albums() {
        // album/ep never require a flagged single.
        assert!(single_song_rule_satisfied("album", 0));
        assert!(single_song_rule_satisfied("ep", 0));
        // a single needs at least one.
        assert!(!single_song_rule_satisfied("single", 0));
        assert!(single_song_rule_satisfied("single", 1));
        assert!(single_song_rule_satisfied("single", 3));
    }
}
