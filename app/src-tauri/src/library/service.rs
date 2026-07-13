//! Library service: server-backed reads when online, cache-only fallback
//! when offline. Each result row carries a `downloaded` flag.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::merged::{MergedAlbum, MergedArtist, MergedTrack};
use crate::auth::AuthManager;
use crate::cache::model as cache_model;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::transport::MetadataEdit;

/// Which data source serviced a call. Surfaced to the UI for diagnostics
/// (e.g. show "offline" badge when source is `Cache`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LibrarySource {
    Server,
    Cache,
}

/// Wrapped result so callers can branch on source without re-asking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryView<T> {
    pub source: LibrarySource,
    pub items: Vec<T>,
    /// Server-reported total when paginating list endpoints. `None` for
    /// search responses (server doesn't return one) and cache-only views.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

impl<T> LibraryView<T> {
    pub(crate) fn server_with_total(items: Vec<T>, total: i64) -> Self {
        Self {
            source: LibrarySource::Server,
            items,
            total: Some(total),
        }
    }
    pub(crate) fn server(items: Vec<T>) -> Self {
        Self {
            source: LibrarySource::Server,
            items,
            total: None,
        }
    }
    pub(crate) fn cache(items: Vec<T>) -> Self {
        Self {
            source: LibrarySource::Cache,
            items,
            total: None,
        }
    }
}

pub struct LibraryService<'a> {
    pool: &'a SqlitePool,
    auth: Arc<AuthManager>,
}

impl<'a> LibraryService<'a> {
    pub fn new(pool: &'a SqlitePool, auth: Arc<AuthManager>) -> Self {
        Self { pool, auth }
    }

    // ----- artists -------------------------------------------------------

    pub async fn list_artists(
        &self,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedArtist>> {
        match self.try_server_list_artists(limit, offset).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_artists: server unavailable, serving cache");
                self.list_artists_from_cache().await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list_artists(
        &self,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedArtist>> {
        let cred = self.auth.credential().await?;
        let (artists, total) = self
            .auth
            .server()
            .list_artists(&cred, limit, offset)
            .await?;
        let downloaded = self
            .downloaded_artist_ids(&artists.iter().map(|a| a.id.as_str()).collect::<Vec<_>>())
            .await?;
        let items = artists
            .into_iter()
            .map(|a| {
                let d = downloaded.contains(&a.id);
                MergedArtist::from_server(a, d)
            })
            .collect();
        Ok(LibraryView::server_with_total(items, total))
    }

    async fn list_artists_from_cache(&self) -> AppResult<LibraryView<MergedArtist>> {
        let rows = repo::list_artists(self.pool).await?;
        Ok(LibraryView::cache(
            rows.into_iter().map(MergedArtist::from_cache).collect(),
        ))
    }

    pub async fn search_artists(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedArtist>> {
        match self.try_server_search_artists(query, limit, offset).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "search_artists: server unavailable, searching cache");
                self.search_artists_from_cache(query).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_search_artists(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedArtist>> {
        let cred = self.auth.credential().await?;
        let artists = self
            .auth
            .server()
            .search_artists(&cred, query, limit, offset)
            .await?;
        let downloaded = self
            .downloaded_artist_ids(&artists.iter().map(|a| a.id.as_str()).collect::<Vec<_>>())
            .await?;
        Ok(LibraryView::server(
            artists
                .into_iter()
                .map(|a| {
                    let d = downloaded.contains(&a.id);
                    MergedArtist::from_server(a, d)
                })
                .collect(),
        ))
    }

    async fn search_artists_from_cache(&self, query: &str) -> AppResult<LibraryView<MergedArtist>> {
        let rows = repo::list_artists(self.pool).await?;
        let q = query.to_ascii_lowercase();
        let items: Vec<MergedArtist> = rows
            .into_iter()
            .filter(|r| r.name.to_ascii_lowercase().contains(&q))
            .map(MergedArtist::from_cache)
            .collect();
        Ok(LibraryView::cache(items))
    }

    /// Fetch a single artist, server-first (with its full alias set) and
    /// falling back to the offline cache (which carries no aliases). Used by
    /// the Artist route to show the canonical name + "Also known as" + drive
    /// the merge/alias controls.
    pub async fn get_artist(&self, id: &str) -> AppResult<MergedArtist> {
        match self.try_server_get_artist(id).await {
            Ok(Some(a)) => Ok(a),
            Ok(None) => self.get_artist_from_cache(id).await,
            Err(e) if is_offline_signal(&e) => self.get_artist_from_cache(id).await,
            Err(e) => Err(e),
        }
    }

    async fn try_server_get_artist(&self, id: &str) -> AppResult<Option<MergedArtist>> {
        let cred = self.auth.credential().await?;
        match self.auth.server().get_artist(&cred, id).await? {
            Some(a) => Ok(Some(self.to_merged_artist(a).await?)),
            None => Ok(None),
        }
    }

    async fn get_artist_from_cache(&self, id: &str) -> AppResult<MergedArtist> {
        let a = repo::get_artist(self.pool, id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("artist {id} not found")))?;
        Ok(MergedArtist::from_cache(a))
    }

    // ----- storage -------------------------------------------------------

    /// The server's library-storage breakdown (homepage widget). This is a
    /// live server view — there's no offline mirror, so it surfaces the
    /// transport error when offline and the UI degrades to "—".
    pub async fn get_library_storage(&self) -> AppResult<crate::transport::LibraryStorage> {
        let cred = self.auth.credential().await?;
        self.auth.server().get_library_storage(&cred).await
    }

    // ----- albums --------------------------------------------------------

    pub async fn list_albums_by_artist(
        &self,
        artist_id: &str,
    ) -> AppResult<LibraryView<MergedAlbum>> {
        match self.try_server_list_albums_by_artist(artist_id).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_albums_by_artist: server unavailable, serving cache");
                self.list_albums_by_artist_from_cache(artist_id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list_albums_by_artist(
        &self,
        artist_id: &str,
    ) -> AppResult<LibraryView<MergedAlbum>> {
        let cred = self.auth.credential().await?;
        let albums = self
            .auth
            .server()
            .list_albums_by_artist(&cred, artist_id)
            .await?;
        let cover_lookup = self
            .local_covers(&albums.iter().map(|a| a.id.as_str()).collect::<Vec<_>>())
            .await?;
        let items = albums
            .into_iter()
            .map(|a| {
                let local = cover_lookup.get(&a.id).cloned();
                let downloaded = local.is_some();
                MergedAlbum::from_server(a, local, downloaded)
            })
            .collect();
        Ok(LibraryView::server(items))
    }

    async fn list_albums_by_artist_from_cache(
        &self,
        artist_id: &str,
    ) -> AppResult<LibraryView<MergedAlbum>> {
        let albums = repo::list_albums_by_artist(self.pool, artist_id).await?;
        let mut out = Vec::with_capacity(albums.len());
        for a in albums {
            let art = repo::get_album_art(self.pool, &a.id).await?;
            out.push(MergedAlbum::from_cache(a, art));
        }
        Ok(LibraryView::cache(out))
    }

    /// Fetch a single album, server-first (with its alias set) and cache
    /// fallback. Used by the Album route for the title + "Also known as" +
    /// merge controls.
    pub async fn get_album(&self, id: &str) -> AppResult<MergedAlbum> {
        match self.try_server_get_album(id).await {
            Ok(Some(a)) => Ok(a),
            Ok(None) => self.get_album_from_cache(id).await,
            Err(e) if is_offline_signal(&e) => self.get_album_from_cache(id).await,
            Err(e) => Err(e),
        }
    }

    async fn try_server_get_album(&self, id: &str) -> AppResult<Option<MergedAlbum>> {
        let cred = self.auth.credential().await?;
        match self.auth.server().get_album(&cred, id).await? {
            Some(a) => Ok(Some(self.to_merged_album(a).await?)),
            None => Ok(None),
        }
    }

    async fn get_album_from_cache(&self, id: &str) -> AppResult<MergedAlbum> {
        let a = repo::get_album(self.pool, id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("album {id} not found")))?;
        let art = repo::get_album_art(self.pool, id).await?;
        Ok(MergedAlbum::from_cache(a, art))
    }

    pub async fn search_albums(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedAlbum>> {
        match self.try_server_search_albums(query, limit, offset).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "search_albums: server unavailable, searching cache");
                self.search_albums_from_cache(query).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_search_albums(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedAlbum>> {
        let cred = self.auth.credential().await?;
        let albums = self
            .auth
            .server()
            .search_albums(&cred, query, limit, offset)
            .await?;
        let cover_lookup = self
            .local_covers(&albums.iter().map(|a| a.id.as_str()).collect::<Vec<_>>())
            .await?;
        let items = albums
            .into_iter()
            .map(|a| {
                let local = cover_lookup.get(&a.id).cloned();
                let downloaded = local.is_some();
                MergedAlbum::from_server(a, local, downloaded)
            })
            .collect();
        Ok(LibraryView::server(items))
    }

    async fn search_albums_from_cache(&self, query: &str) -> AppResult<LibraryView<MergedAlbum>> {
        // No global "list albums" repo fn yet — pull all artists then their
        // albums. The cache only contains downloaded content, so the cost
        // is bounded.
        let artists = repo::list_artists(self.pool).await?;
        let q = query.to_ascii_lowercase();
        let mut out = Vec::new();
        for artist in artists {
            let albums = repo::list_albums_by_artist(self.pool, &artist.id).await?;
            for a in albums {
                if a.title.to_ascii_lowercase().contains(&q) {
                    let art = repo::get_album_art(self.pool, &a.id).await?;
                    out.push(MergedAlbum::from_cache(a, art));
                }
            }
        }
        Ok(LibraryView::cache(out))
    }

    // ----- tracks --------------------------------------------------------

    pub async fn list_tracks_by_album(
        &self,
        album_id: &str,
    ) -> AppResult<LibraryView<MergedTrack>> {
        match self.try_server_list_tracks_by_album(album_id).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_tracks_by_album: server unavailable, serving cache");
                self.list_tracks_by_album_from_cache(album_id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list_tracks_by_album(
        &self,
        album_id: &str,
    ) -> AppResult<LibraryView<MergedTrack>> {
        let cred = self.auth.credential().await?;
        let tracks = self
            .auth
            .server()
            .list_tracks_by_album(&cred, album_id)
            .await?;
        let local = self
            .local_track_paths(&tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>())
            .await?;
        let items = tracks
            .into_iter()
            .map(|t| {
                let lp = local.get(&t.id).cloned();
                MergedTrack::from_server(t, lp)
            })
            .collect();
        Ok(LibraryView::server(items))
    }

    async fn list_tracks_by_album_from_cache(
        &self,
        album_id: &str,
    ) -> AppResult<LibraryView<MergedTrack>> {
        let rows = repo::list_tracks_by_album(self.pool, album_id).await?;
        Ok(LibraryView::cache(
            rows.into_iter().map(MergedTrack::from_cache).collect(),
        ))
    }

    pub async fn search_tracks(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedTrack>> {
        match self.try_server_search_tracks(query, limit, offset).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "search_tracks: server unavailable, searching cache");
                self.search_tracks_from_cache(query).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_search_tracks(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<LibraryView<MergedTrack>> {
        let cred = self.auth.credential().await?;
        let tracks = self
            .auth
            .server()
            .search_tracks(&cred, query, limit, offset)
            .await?;
        let local = self
            .local_track_paths(&tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>())
            .await?;
        let items = tracks
            .into_iter()
            .map(|t| {
                let lp = local.get(&t.id).cloned();
                MergedTrack::from_server(t, lp)
            })
            .collect();
        Ok(LibraryView::server(items))
    }

    async fn search_tracks_from_cache(&self, query: &str) -> AppResult<LibraryView<MergedTrack>> {
        let rows = repo::list_downloaded_tracks(self.pool).await?;
        let q = query.to_ascii_lowercase();
        let items: Vec<MergedTrack> = rows
            .into_iter()
            .filter(|t| t.title.to_ascii_lowercase().contains(&q))
            .map(MergedTrack::from_cache)
            .collect();
        Ok(LibraryView::cache(items))
    }

    // ----- metadata edit (Phase 9) ---------------------------------------

    /// Apply an opt-in metadata edit to a track (Manager+, enforced
    /// server-side). The server is authoritative: the edit is pushed first,
    /// and on success — if the track is downloaded — the change is mirrored
    /// into the offline cache so a downloaded item reflects the edit before
    /// the next sync reconcile (which would also pick it up via the
    /// content-hash compare in `sync::engine`). The client-owned fields
    /// (`local_file_path`, `downloaded_at`) are preserved.
    ///
    /// Requires a live server: a metadata edit cannot be queued offline (the
    /// audit/rollback contract is server-owned), so transport failures
    /// surface to the caller rather than falling back to the cache.
    pub async fn edit_track_metadata(
        &self,
        id: &str,
        edit: &MetadataEdit,
    ) -> AppResult<MergedTrack> {
        let updated = self.auth.edit_track_metadata(id, edit).await?;

        // Mirror into the cache only when the track is already downloaded.
        let local_file_path = match repo::get_track(self.pool, id).await? {
            Some(existing) => {
                let row = cache_model::Track {
                    id: updated.id.clone(),
                    album_id: updated.album_id.clone(),
                    artist_id: updated.artist_id.clone(),
                    title: updated.title.clone(),
                    track_no: updated.track_no,
                    disc_no: updated.disc_no,
                    duration_ms: updated.duration_ms,
                    codec: updated.codec.clone(),
                    bitrate_kbps: updated.bitrate_kbps,
                    file_size: updated.file_size,
                    sample_rate_hz: updated.sample_rate_hz,
                    bit_depth: updated.bit_depth,
                    channels: updated.channels,
                    loudness_lufs: updated.loudness_lufs,
                    loudness_peak: updated.loudness_peak,
                    album_loudness_lufs: updated.album_loudness_lufs,
                    // Client-owned — never overwritten by a metadata edit.
                    local_file_path: existing.local_file_path.clone(),
                    metadata_json: updated.metadata_json.clone(),
                    downloaded_at: existing.downloaded_at,
                    updated_at: now_iso(),
                };
                repo::upsert_track(self.pool, &row).await?;
                Some(existing.local_file_path)
            }
            None => None,
        };

        Ok(MergedTrack::from_server(updated, local_file_path))
    }

    // ----- Merge + aliases (Phase 10; Manager+, server-authoritative) ------
    //
    // These re-shape the catalog (re-point children, delete a row, preserve
    // spellings), so they require a live server and aren't queued offline. The
    // server result is returned as a Merged* entity; the offline cache
    // reconciles on the next sync (the UI also invalidates + refetches).

    pub async fn merge_artists(
        &self,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<MergedArtist> {
        let a = self.auth.merge_artists(survivor_id, duplicate_id).await?;
        self.to_merged_artist(a).await
    }

    pub async fn merge_albums(
        &self,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<MergedAlbum> {
        let a = self.auth.merge_albums(survivor_id, duplicate_id).await?;
        self.to_merged_album(a).await
    }

    /// List the on-disk `<Language>/<Artist>` directories an artist occupies,
    /// plus the library's existing language folders.
    pub async fn list_artist_library_paths(
        &self,
        artist_id: &str,
    ) -> AppResult<crate::transport::ArtistStoragePaths> {
        self.auth.list_artist_library_paths(artist_id).await
    }

    /// Move all of an artist's tracks under a single language folder.
    pub async fn set_artist_language(
        &self,
        artist_id: &str,
        target_language: &str,
        target_folder: Option<&str>,
    ) -> AppResult<crate::transport::RelocateReport> {
        self.auth
            .set_artist_language(artist_id, target_language, target_folder)
            .await
    }

    /// Inspect an album's on-disk folder (current name + title-based suggestion).
    pub async fn album_folder(
        &self,
        album_id: &str,
    ) -> AppResult<crate::transport::AlbumFolderInfo> {
        self.auth.album_folder(album_id).await
    }

    /// Rename an album's on-disk folder (empty/None → match the album title).
    pub async fn rename_album_folder(
        &self,
        album_id: &str,
        folder_name: Option<&str>,
    ) -> AppResult<crate::transport::RelocateReport> {
        self.auth.rename_album_folder(album_id, folder_name).await
    }

    pub async fn move_track(
        &self,
        track_id: &str,
        album_id: &str,
        single_release: bool,
    ) -> AppResult<MergedTrack> {
        let t = self
            .auth
            .move_track(track_id, album_id, single_release)
            .await?;
        self.to_merged_track(t).await
    }

    pub async fn set_track_single_release(
        &self,
        track_id: &str,
        single_release: bool,
    ) -> AppResult<MergedTrack> {
        let t = self
            .auth
            .set_track_single_release(track_id, single_release)
            .await?;
        self.to_merged_track(t).await
    }

    pub async fn set_track_explicit(
        &self,
        track_id: &str,
        explicit: bool,
    ) -> AppResult<MergedTrack> {
        let t = self.auth.set_track_explicit(track_id, explicit).await?;
        self.to_merged_track(t).await
    }

    pub async fn set_album_type(
        &self,
        album_id: &str,
        album_type: &str,
        single_track_id: Option<&str>,
    ) -> AppResult<MergedAlbum> {
        let a = self
            .auth
            .set_album_type(album_id, album_type, single_track_id)
            .await?;
        self.to_merged_album(a).await
    }

    pub async fn add_artist_alias(
        &self,
        artist_id: &str,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> AppResult<MergedArtist> {
        let a = self
            .auth
            .add_artist_alias(artist_id, name, sort_name, language)
            .await?;
        self.to_merged_artist(a).await
    }

    pub async fn remove_artist_alias(
        &self,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedArtist> {
        let a = self.auth.remove_artist_alias(artist_id, alias_id).await?;
        self.to_merged_artist(a).await
    }

    pub async fn set_primary_artist_alias(
        &self,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedArtist> {
        let a = self
            .auth
            .set_primary_artist_alias(artist_id, alias_id)
            .await?;
        self.to_merged_artist(a).await
    }

    pub async fn add_album_alias(
        &self,
        album_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<MergedAlbum> {
        let a = self.auth.add_album_alias(album_id, title, language).await?;
        self.to_merged_album(a).await
    }

    pub async fn remove_album_alias(
        &self,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedAlbum> {
        let a = self.auth.remove_album_alias(album_id, alias_id).await?;
        self.to_merged_album(a).await
    }

    pub async fn set_primary_album_alias(
        &self,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedAlbum> {
        let a = self
            .auth
            .set_primary_album_alias(album_id, alias_id)
            .await?;
        self.to_merged_album(a).await
    }

    pub async fn list_track_aliases(
        &self,
        track_id: &str,
    ) -> AppResult<Vec<crate::transport::AliasInfo>> {
        self.auth.list_track_aliases(track_id).await
    }

    pub async fn add_track_alias(
        &self,
        track_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<MergedTrack> {
        let t = self.auth.add_track_alias(track_id, title, language).await?;
        self.to_merged_track(t).await
    }

    pub async fn remove_track_alias(
        &self,
        track_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedTrack> {
        let t = self.auth.remove_track_alias(track_id, alias_id).await?;
        self.to_merged_track(t).await
    }

    pub async fn set_primary_track_alias(
        &self,
        track_id: &str,
        alias_id: &str,
    ) -> AppResult<MergedTrack> {
        let t = self
            .auth
            .set_primary_track_alias(track_id, alias_id)
            .await?;
        self.to_merged_track(t).await
    }

    // ----- helpers -------------------------------------------------------

    /// Build a `MergedArtist` from a server row, filling the `downloaded` flag
    /// from the offline cache.
    async fn to_merged_artist(&self, a: crate::transport::Artist) -> AppResult<MergedArtist> {
        let downloaded = self
            .downloaded_artist_ids(&[a.id.as_str()])
            .await?
            .contains(a.id.as_str());
        Ok(MergedArtist::from_server(a, downloaded))
    }

    /// Build a `MergedAlbum` from a server row, filling local-cover/downloaded.
    async fn to_merged_album(&self, a: crate::transport::Album) -> AppResult<MergedAlbum> {
        let local = self
            .local_covers(&[a.id.as_str()])
            .await?
            .get(&a.id)
            .cloned();
        let downloaded = local.is_some();
        Ok(MergedAlbum::from_server(a, local, downloaded))
    }

    /// Build a `MergedTrack` from a server row, filling the local file path.
    async fn to_merged_track(&self, t: crate::transport::Track) -> AppResult<MergedTrack> {
        let lp = self
            .local_track_paths(&[t.id.as_str()])
            .await?
            .get(&t.id)
            .cloned();
        Ok(MergedTrack::from_server(t, lp))
    }

    /// Which of the given artist IDs have at least one cached track? Used
    /// to populate the `downloaded` flag on server-sourced artist rows.
    async fn downloaded_artist_ids(&self, ids: &[&str]) -> AppResult<HashSet<String>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        // SQLite IN-list — bind each id explicitly. The set is bounded by
        // the server's pagination cap (200), so dynamic SQL is fine here.
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("SELECT DISTINCT artist_id FROM tracks WHERE artist_id IN ({placeholders})");
        let mut q = sqlx::query_scalar::<_, String>(&sql);
        for id in ids {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(self.pool).await?;
        Ok(rows.into_iter().collect())
    }

    /// Local cover paths for the given album IDs.
    async fn local_covers(&self, ids: &[&str]) -> AppResult<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT album_id, local_cover_path FROM album_art WHERE album_id IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (String, String)>(&sql);
        for id in ids {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(self.pool).await?;
        Ok(rows.into_iter().collect())
    }

    /// `local_file_path` per track id, for the given ids. Only tracks
    /// actually downloaded are returned.
    async fn local_track_paths(&self, ids: &[&str]) -> AppResult<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id, local_file_path FROM tracks WHERE id IN ({placeholders})");
        let mut q = sqlx::query_as::<_, (String, String)>(&sql);
        for id in ids {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(self.pool).await?;
        Ok(rows.into_iter().collect())
    }
}

/// True if an error should trigger a cache fallback. We fall back on
/// transport errors (server unreachable, codec, timeout) and when no
/// credential is configured locally. We do NOT fall back on auth
/// rejections — those mean the user needs to re-auth, not that we should
/// silently serve stale data.
fn is_offline_signal(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_) | AppError::AuthNotConfigured(_))
}

/// RFC3339 timestamp for cache `updated_at` stamps on an optimistic mirror.
fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
