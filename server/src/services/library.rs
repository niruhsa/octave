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
    self as m, Album, AlbumAlias, Artist, ArtistAlias, NewAlbum, NewAlbumAlias, NewArtist,
    NewArtistAlias, NewAuditEntry, NewTrack, PermissionLevel, Track,
};
use crate::db::repo::{AliasRepo, AlbumRepo, ArtistRepo, AuditRepo, FollowRepo, TrackRepo};
use crate::error::{AppError, Result};
use crate::services::notification::NotificationService;
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
        let after = self
            .tracks
            .set_single_release(track_id, single_release)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        self.audit(caller, "track.update", "track", Some(track_id), Some(&before), Some(&after))
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

#[cfg(test)]
mod tests {
    use super::{pick_primary_index, resolve_language};

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
}
