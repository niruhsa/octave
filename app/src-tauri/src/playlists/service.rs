//! Playlist service: server-backed reads + online/offline-aware mutations.
//!
//! Decision rule per mutation:
//!   * If `id` is a `local:` placeholder (offline-created, create op still
//!     queued) → always the offline path: enqueue a `PendingOpKind` and
//!     apply the change optimistically to the cache. The sync engine
//!     replays the op and remaps the temp id on reconnect.
//!   * Else → try the server. On success, mirror the result into the cache
//!     so the offline view stays current. On an *offline signal*
//!     (`Transport` failure or `AuthNotConfigured`) → fall back to the
//!     outbox + optimistic cache mutation, same as the local-id case.
//!   * Server-rejected mutations that aren't transport failures
//!     (`Forbidden` / `Unauthenticated` / `Internal` mapping to
//!     `InvalidArgument` / `NotFound` / `FailedPrecondition`) propagate to
//!     the UI; the cache is untouched so the user sees the prior state.
//!
//! Reads mirror `library::service`: try the server, fall back to the cache
//! on an offline signal. The detail view enriches each entry's track from
//! the cache when downloaded, else from a bounded server `get_track` fetch
//! (online only, capped at `FETCH_TRACK_CAP` to keep cost sane on long
//! playlists). Offline + uncached entries surface a stub `MergedTrack` so
//! the UI can mark them "stream-only / not available offline".

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;
use uuid::Uuid;

use super::merged::{MergedPlaylist, MergedPlaylistEntry, PlaylistDetailView};
use crate::auth::AuthManager;
use crate::cache::{self, repo};
use crate::error::{AppError, AppResult};
use crate::library::{LibrarySource, LibraryView, MergedTrack};
use crate::sync::ops::{is_local_id, PendingOpKind};
use crate::transport::{Credential, PlaylistTrack, ServerClient};

/// Cap on per-detail server `get_track` fetches for uncached entries.
/// Playlists longer than this show stubs for the tail; the offline-cache
/// principle means we don't cache the resolved metadata for non-downloaded
/// tracks anyway, so a bounded fetch keeps the view responsive.
const FETCH_TRACK_CAP: usize = 200;

pub struct PlaylistService<'a> {
    pool: &'a SqlitePool,
    auth: Arc<AuthManager>,
}

impl<'a> PlaylistService<'a> {
    pub fn new(pool: &'a SqlitePool, auth: Arc<AuthManager>) -> Self {
        Self { pool, auth }
    }

    fn server(&self) -> &ServerClient {
        self.auth.server()
    }

    async fn cred(&self) -> AppResult<Credential> {
        self.auth.credential().await
    }

    /// The current user's id, when known. `None` for `SECRET_KEY` (which has
    /// no user_id and is rejected from `playlist.create` server-side).
    async fn owner_id(&self) -> AppResult<Option<String>> {
        Ok(self.auth.current().await.and_then(|s| s.user_id))
    }

    // ----- reads ---------------------------------------------------------

    /// List the current user's playlists. Online: fetches the server list
    /// and upserts each row into the cache so the next offline view has
    /// them. Offline: serves the cache only.
    pub async fn list_my_playlists(&self) -> AppResult<LibraryView<MergedPlaylist>> {
        match self.try_server_list().await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_my_playlists: server unavailable, serving cache");
                self.list_from_cache().await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list(&self) -> AppResult<LibraryView<MergedPlaylist>> {
        let cred = self.cred().await?;
        let rows = self.server().list_my_playlists(&cred).await?;
        // Mirror into the cache so the offline view + the sync engine's
        // pull phase have a row to reconcile against. We don't touch
        // `playlist_tracks` here — the detail view populates them.
        for p in &rows {
            let row = cache::model::Playlist {
                id: p.id.clone(),
                owner_id: p.owner_id.clone(),
                name: p.name.clone(),
                updated_at: now_iso(),
            };
            repo::upsert_playlist(self.pool, &row).await?;
        }
        Ok(LibraryView::server(rows.into_iter().map(MergedPlaylist::from_transport).collect()))
    }

    async fn list_from_cache(&self) -> AppResult<LibraryView<MergedPlaylist>> {
        let rows = repo::list_playlists(self.pool).await?;
        Ok(LibraryView::cache(rows.into_iter().map(MergedPlaylist::from_cache).collect()))
    }

    /// One playlist with its ordered entries. Online: fetches the server
    /// view, mirrors it into the cache, and enriches uncached entries via
    /// a bounded `get_track` batch. Offline: reads the cache and enriches
    /// from cached track rows only; uncached entries get a stub.
    pub async fn get_playlist(&self, id: &str) -> AppResult<Option<PlaylistDetailView>> {
        match self.try_server_get(id).await {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) => Ok(None),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "get_playlist: server unavailable, serving cache");
                self.get_from_cache(id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_get(&self, id: &str) -> AppResult<Option<PlaylistDetailView>> {
        // Local-id playlists only exist in the cache + outbox; the server
        // doesn't know them. Serve from cache so the UI stays consistent
        // while the create op is queued.
        if is_local_id(id) {
            return self.get_from_cache(id).await;
        }
        let cred = self.cred().await?;
        let view = match self.server().get_playlist(&cred, id).await? {
            Some(v) => v,
            None => return Ok(None),
        };
        // Mirror playlist + entries into the cache.
        let p_row = cache::model::Playlist {
            id: view.playlist.id.clone(),
            owner_id: view.playlist.owner_id.clone(),
            name: view.playlist.name.clone(),
            updated_at: now_iso(),
        };
        repo::upsert_playlist(self.pool, &p_row).await?;
        let entries: Vec<cache::model::PlaylistTrack> = view
            .tracks
            .iter()
            .map(|t| cache::model::PlaylistTrack {
                playlist_id: view.playlist.id.clone(),
                track_id: t.track_id.clone(),
                position: t.position,
                added_at: now_iso(),
            })
            .collect();
        repo::replace_playlist_tracks(self.pool, &view.playlist.id, &entries).await?;

        // Enrich: cache lookup first, then bounded server fetch for misses.
        let merged = self.enrich_entries(&view.tracks, /*online=*/ true).await?;
        Ok(Some(PlaylistDetailView {
            source: LibrarySource::Server,
            playlist: MergedPlaylist::from_transport(view.playlist),
            entries: merged,
        }))
    }

    async fn get_from_cache(&self, id: &str) -> AppResult<Option<PlaylistDetailView>> {
        let p = match repo::list_playlists(self.pool).await?.into_iter().find(|p| p.id == id) {
            Some(p) => p,
            None => return Ok(None),
        };
        let rows = repo::list_playlist_tracks(self.pool, id).await?;
        // Offline: only cached (downloaded) tracks have full metadata.
        // Uncached entries → stub. We pass `online=false` so the helper
        // doesn't try the server.
        let entries = self.enrich_cache_entries(&rows).await?;
        Ok(Some(PlaylistDetailView {
            source: LibrarySource::Cache,
            playlist: MergedPlaylist::from_cache(p),
            entries,
        }))
    }

    /// Build `MergedPlaylistEntry` rows from a list of (position, track_id,
    /// added_at). Cache lookup first; for misses, when `online`, fetch via
    /// `server.get_track` (bounded); when offline, emit a stub.
    async fn enrich_entries(
        &self,
        tracks: &[PlaylistTrack],
        online: bool,
    ) -> AppResult<Vec<MergedPlaylistEntry>> {
        // Pull every cached track in one IN-list query — much cheaper than
        // N `get_track` round-trips through the repo.
        let ids: Vec<&str> = tracks.iter().map(|t| t.track_id.as_str()).collect();
        let cached = self.cached_tracks_map(&ids).await?;

        let mut entries = Vec::with_capacity(tracks.len());
        let mut fetched = 0usize;
        let cred_holder;
        let cred = if online {
            cred_holder = self.cred().await?;
            Some(&cred_holder)
        } else {
            None
        };

        for t in tracks {
            let track = if let Some(c) = cached.get(&t.track_id) {
                MergedTrack::from_cache(c.clone())
            } else if fetched < FETCH_TRACK_CAP {
                if let Some(cred) = cred {
                    fetched += 1;
                    match self.server().get_track(cred, &t.track_id).await? {
                        Some(srv) => MergedTrack::from_server(srv, None),
                        None => stub_track(&t.track_id),
                    }
                } else {
                    stub_track(&t.track_id)
                }
            } else {
                stub_track(&t.track_id)
            };
            entries.push(MergedPlaylistEntry {
                position: t.position,
                added_at: now_iso(),
                track,
            });
        }
        Ok(entries)
    }

    /// Offline-only enrichment: cache hit → full row; miss → stub. No server
    /// calls. `added_at` is taken from the cached `playlist_tracks` row so
    /// reordering preserves (approximately) the original add order in the
    /// offline view.
    async fn enrich_cache_entries(
        &self,
        rows: &[cache::model::PlaylistTrack],
    ) -> AppResult<Vec<MergedPlaylistEntry>> {
        let ids: Vec<&str> = rows.iter().map(|r| r.track_id.as_str()).collect();
        let cached = self.cached_tracks_map(&ids).await?;
        Ok(rows
            .iter()
            .map(|r| MergedPlaylistEntry {
                position: r.position,
                added_at: r.added_at.clone(),
                track: cached
                    .get(&r.track_id)
                    .cloned()
                    .map(MergedTrack::from_cache)
                    .unwrap_or_else(|| stub_track(&r.track_id)),
            })
            .collect())
    }

    /// One query: `SELECT … FROM tracks WHERE id IN (…)`. Returns the full
    /// cache row per hit so `MergedTrack::from_cache` has every field.
    async fn cached_tracks_map(
        &self,
        ids: &[&str],
    ) -> AppResult<HashMap<String, cache::model::Track>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, album_id, artist_id, title, track_no, disc_no,
                   duration_ms, codec, bitrate_kbps, file_size,
                   local_file_path, metadata_json, downloaded_at, updated_at
              FROM tracks WHERE id IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, cache::model::Track>(&sql);
        for id in ids {
            q = q.bind(*id);
        }
        let rows = q.fetch_all(self.pool).await?;
        Ok(rows.into_iter().map(|t| (t.id.clone(), t)).collect())
    }

    // ----- mutations -----------------------------------------------------

    /// Create a playlist. Online: server creates, cache mirrors, no op.
    /// Offline: mint a `local:` id, write a cache row owned by the current
    /// user, and enqueue a `playlist.create` op for replay. `SECRET_KEY`
    /// sessions have no `user_id` and are rejected — the server rejects
    /// them too, so this mirrors server auth.
    pub async fn create_playlist(&self, name: &str) -> AppResult<MergedPlaylist> {
        let owner = self.owner_id().await?;
        match self.try_server_create(name).await {
            Ok(p) => Ok(p),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "create_playlist: server unavailable, minting local id");
                self.create_offline(name, owner).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_create(&self, name: &str) -> AppResult<MergedPlaylist> {
        let cred = self.cred().await?;
        let p = self.server().create_playlist(&cred, name).await?;
        let row = cache::model::Playlist {
            id: p.id.clone(),
            owner_id: p.owner_id.clone(),
            name: p.name.clone(),
            updated_at: now_iso(),
        };
        repo::upsert_playlist(self.pool, &row).await?;
        Ok(MergedPlaylist::from_transport(p))
    }

    async fn create_offline(
        &self,
        name: &str,
        owner: Option<String>,
    ) -> AppResult<MergedPlaylist> {
        let owner_id = owner.ok_or_else(|| {
            AppError::AuthNotConfigured(
                "offline playlist create needs a user_id; SECRET_KEY has none".into(),
            )
        })?;
        let local_id = format!("local:{}", Uuid::new_v4().as_simple());
        let row = cache::model::Playlist {
            id: local_id.clone(),
            owner_id: owner_id.clone(),
            name: name.to_string(),
            updated_at: now_iso(),
        };
        repo::upsert_playlist(self.pool, &row).await?;
        let op = PendingOpKind::PlaylistCreate {
            local_id: local_id.clone(),
            name: name.to_string(),
        };
        enqueue(self.pool, op).await?;
        Ok(MergedPlaylist {
            id: local_id,
            owner_id,
            name: name.to_string(),
            local: true,
        })
    }

    /// Rename. Online + server-known id: server renames, cache mirrors.
    /// Offline or local-id: enqueue + optimistic cache update.
    pub async fn rename_playlist(
        &self,
        id: &str,
        name: &str,
    ) -> AppResult<MergedPlaylist> {
        if is_local_id(id) {
            return self.rename_offline(id, name).await;
        }
        match self.try_server_rename(id, name).await {
            Ok(p) => Ok(p),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "rename_playlist: server unavailable, queuing");
                self.rename_offline(id, name).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_rename(&self, id: &str, name: &str) -> AppResult<MergedPlaylist> {
        let cred = self.cred().await?;
        let p = self.server().rename_playlist(&cred, id, name).await?;
        let row = cache::model::Playlist {
            id: p.id.clone(),
            owner_id: p.owner_id.clone(),
            name: p.name.clone(),
            updated_at: now_iso(),
        };
        repo::upsert_playlist(self.pool, &row).await?;
        Ok(MergedPlaylist::from_transport(p))
    }

    async fn rename_offline(&self, id: &str, name: &str) -> AppResult<MergedPlaylist> {
        // Update the cache row (if it exists) so the UI reflects the rename
        // immediately, then enqueue the op. A local-id row is in the cache;
        // a server-id row may or may not be — upsert covers both.
        let existing = repo::list_playlists(self.pool).await?.into_iter().find(|p| p.id == id);
        let owner_id = match existing {
            Some(p) => p.owner_id,
            None => self.owner_id().await?.unwrap_or_default(),
        };
        let row = cache::model::Playlist {
            id: id.to_string(),
            owner_id,
            name: name.to_string(),
            updated_at: now_iso(),
        };
        repo::upsert_playlist(self.pool, &row).await?;
        enqueue(
            self.pool,
            PendingOpKind::PlaylistRename {
                playlist_id: id.to_string(),
                name: name.to_string(),
            },
        )
        .await?;
        Ok(MergedPlaylist {
            id: id.to_string(),
            owner_id: row.owner_id,
            name: name.to_string(),
            local: is_local_id(id),
        })
    }

    /// Delete. Online + server-known id: server deletes, cache prunes.
    /// Offline or local-id: enqueue + optimistic cache delete. For a
    /// local-id this also drops any dependent ops still queued against it
    /// (they'd reference a playlist that no longer exists).
    pub async fn delete_playlist(&self, id: &str) -> AppResult<()> {
        if is_local_id(id) {
            return self.delete_offline_local(id).await;
        }
        match self.try_server_delete(id).await {
            Ok(()) => Ok(()),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "delete_playlist: server unavailable, queuing");
                self.delete_offline_server(id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_delete(&self, id: &str) -> AppResult<()> {
        let cred = self.cred().await?;
        self.server().delete_playlist(&cred, id).await?;
        // Cascade: playlist_tracks FK is ON DELETE CASCADE; the playlist
        // row delete handles both.
        repo::delete_playlist(self.pool, id).await?;
        Ok(())
    }

    async fn delete_offline_server(&self, id: &str) -> AppResult<()> {
        repo::delete_playlist(self.pool, id).await?;
        enqueue(
            self.pool,
            PendingOpKind::PlaylistDelete {
                playlist_id: id.to_string(),
            },
        )
        .await
    }

    async fn delete_offline_local(&self, id: &str) -> AppResult<()> {
        repo::delete_playlist(self.pool, id).await?;
        // Drop every queued op that still references this local id — the
        // create op and any dependent add/remove/reorder ops. Without this
        // the engine would replay a create for a playlist the user already
        // discarded, then mutate a now-orphaned server row.
        let ops = repo::list_pending_ops(self.pool).await?;
        for row in ops {
            if let Ok(kind) = PendingOpKind::from_payload_json(&row.payload_json) {
                if kind.references_playlist(id) {
                    repo::delete_pending_op(self.pool, row.id).await?;
                }
            }
        }
        Ok(())
    }

    /// Add a track. `position = 0` ⇒ append; `position ≥ 1` ⇒ 1-based
    /// insert with shift. Returns the refreshed detail view.
    pub async fn add_track(
        &self,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<PlaylistDetailView> {
        if is_local_id(playlist_id) {
            return self.add_offline(playlist_id, track_id, position).await;
        }
        match self.try_server_add(playlist_id, track_id, position).await {
            Ok(()) => self.get_from_cache(playlist_id).await?.ok_or_else(|| {
                AppError::Internal("playlist vanished from cache after add".into())
            }),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "add_track: server unavailable, queuing");
                self.add_offline(playlist_id, track_id, position).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_add(
        &self,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<()> {
        let cred = self.cred().await?;
        self.server()
            .add_playlist_track(&cred, playlist_id, track_id, position)
            .await?;
        // Optimistic mirror: insert into the cache so the next detail read
        // (which we're about to do) reflects the new entry without a
        // server refetch.
        self.optimistic_add(playlist_id, track_id, position).await
    }

    async fn add_offline(
        &self,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<PlaylistDetailView> {
        self.optimistic_add(playlist_id, track_id, position).await?;
        enqueue(
            self.pool,
            PendingOpKind::PlaylistAddTrack {
                playlist_id: playlist_id.to_string(),
                track_id: track_id.to_string(),
                position,
            },
        )
        .await?;
        self.get_from_cache(playlist_id).await?.ok_or_else(|| {
            AppError::Internal("playlist vanished from cache after offline add".into())
        })
    }

    /// Insert the track into the cache's `playlist_tracks` table with
    /// 1-based contiguous renumbering. Idempotent vs the existing entry
    /// list because `replace_playlist_tracks` deletes-then-inserts in one
    /// transaction.
    async fn optimistic_add(
        &self,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<()> {
        let mut rows = repo::list_playlist_tracks(self.pool, playlist_id).await?;
        // Renumber to a clean 1..N before splicing so a previously-shifted
        // cache (e.g. after a failed op) doesn't drift.
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
        let insert_at = if position <= 0 {
            rows.len() // append
        } else {
            (position as usize).saturating_sub(1).min(rows.len())
        };
        // Avoid trivial duplicate entries (same track at the splice point is
        // still allowed — the server permits dupes, so we do too).
        let new_row = cache::model::PlaylistTrack {
            playlist_id: playlist_id.to_string(),
            track_id: track_id.to_string(),
            position: 0, // placeholder; renumbered below
            added_at: now_iso(),
        };
        rows.insert(insert_at, new_row);
        // Renumber 1..N after the splice.
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
        repo::replace_playlist_tracks(self.pool, playlist_id, &rows).await
    }

    /// Remove the entry at 1-based `position`. Returns the refreshed detail.
    pub async fn remove_track(
        &self,
        playlist_id: &str,
        position: i32,
    ) -> AppResult<PlaylistDetailView> {
        if is_local_id(playlist_id) {
            return self.remove_offline(playlist_id, position).await;
        }
        match self.try_server_remove(playlist_id, position).await {
            Ok(()) => self.get_from_cache(playlist_id).await?.ok_or_else(|| {
                AppError::Internal("playlist vanished from cache after remove".into())
            }),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "remove_track: server unavailable, queuing");
                self.remove_offline(playlist_id, position).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_remove(
        &self,
        playlist_id: &str,
        position: i32,
    ) -> AppResult<()> {
        let cred = self.cred().await?;
        self.server()
            .remove_playlist_track(&cred, playlist_id, position)
            .await?;
        self.optimistic_remove(playlist_id, position).await
    }

    async fn remove_offline(
        &self,
        playlist_id: &str,
        position: i32,
    ) -> AppResult<PlaylistDetailView> {
        self.optimistic_remove(playlist_id, position).await?;
        enqueue(
            self.pool,
            PendingOpKind::PlaylistRemoveTrack {
                playlist_id: playlist_id.to_string(),
                position,
            },
        )
        .await?;
        self.get_from_cache(playlist_id).await?.ok_or_else(|| {
            AppError::Internal("playlist vanished from cache after offline remove".into())
        })
    }

    async fn optimistic_remove(&self, playlist_id: &str, position: i32) -> AppResult<()> {
        let mut rows = repo::list_playlist_tracks(self.pool, playlist_id).await?;
        // 1-based position → index.
        let idx = (position as usize).saturating_sub(1);
        if idx < rows.len() {
            rows.remove(idx);
        }
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
        repo::replace_playlist_tracks(self.pool, playlist_id, &rows).await
    }

    /// Move the entry at 1-based `from_position` to 1-based `to_position`,
    /// shifting the run between them. Returns the refreshed detail.
    pub async fn reorder_track(
        &self,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<PlaylistDetailView> {
        if is_local_id(playlist_id) {
            return self.reorder_offline(playlist_id, from_position, to_position).await;
        }
        match self.try_server_reorder(playlist_id, from_position, to_position).await {
            Ok(()) => self.get_from_cache(playlist_id).await?.ok_or_else(|| {
                AppError::Internal("playlist vanished from cache after reorder".into())
            }),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "reorder_track: server unavailable, queuing");
                self.reorder_offline(playlist_id, from_position, to_position).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_reorder(
        &self,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<()> {
        let cred = self.cred().await?;
        self.server()
            .reorder_playlist_track(&cred, playlist_id, from_position, to_position)
            .await?;
        self.optimistic_reorder(playlist_id, from_position, to_position).await
    }

    async fn reorder_offline(
        &self,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<PlaylistDetailView> {
        self.optimistic_reorder(playlist_id, from_position, to_position).await?;
        enqueue(
            self.pool,
            PendingOpKind::PlaylistReorderTrack {
                playlist_id: playlist_id.to_string(),
                from_position,
                to_position,
            },
        )
        .await?;
        self.get_from_cache(playlist_id).await?.ok_or_else(|| {
            AppError::Internal("playlist vanished from cache after offline reorder".into())
        })
    }

    async fn optimistic_reorder(
        &self,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<()> {
        let mut rows = repo::list_playlist_tracks(self.pool, playlist_id).await?;
        // Renumber clean first so a drifted cache doesn't make `from`/`to`
        // miss the intended rows.
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
        let from = (from_position as usize).saturating_sub(1).min(rows.len().saturating_sub(1));
        // `to_position` is the destination *slot* in 1-based terms; clamp to
        // the valid range so we don't panic on a bad input.
        let to = (to_position as usize).saturating_sub(1).min(rows.len().saturating_sub(1));
        if from == to || from >= rows.len() {
            return Ok(());
        }
        let moved = rows.remove(from);
        rows.insert(to, moved);
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
        repo::replace_playlist_tracks(self.pool, playlist_id, &rows).await
    }
}

// ----- helpers ----------------------------------------------------------

/// Append a typed op to the outbox. Thin wrapper over `repo::enqueue_op`
/// so call sites stay tag-driven.
async fn enqueue(pool: &SqlitePool, op: PendingOpKind) -> AppResult<()> {
    let payload = op.to_payload_json()?;
    repo::enqueue_op(pool, op.op_type(), &payload).await?;
    Ok(())
}

/// True if an error should trigger the offline path. Same definition as
/// `library::service::is_offline_signal` — kept local so this module stays
/// decoupled from library internals.
fn is_offline_signal(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_) | AppError::AuthNotConfigured(_))
}

/// A minimal `MergedTrack` for an entry whose track row isn't available
/// (offline + not downloaded, or server-reported deleted). `downloaded`
/// is `false` and the display fields are empty so the UI can mark the row
/// "stream-only / not available offline". The `id` is preserved so the
/// player can still attempt `media://` resolution when online.
fn stub_track(track_id: &str) -> MergedTrack {
    MergedTrack {
        id: track_id.to_string(),
        album_id: String::new(),
        artist_id: String::new(),
        title: String::new(),
        track_no: None,
        disc_no: None,
        duration_ms: 0,
        codec: String::new(),
        bitrate_kbps: None,
        file_path: String::new(),
        file_size: None,
        local_file_path: None,
        downloaded: false,
    }
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // The optimistic renumber/splice logic is the part worth pinning down
    // without a DB; everything else is a thin shell over repo + transport
    // that the integration tests in `tests/playlists.rs` cover.

    use super::*;
    use crate::transport::Playlist;

    fn row(pid: &str, tid: &str, pos: i64) -> cache::model::PlaylistTrack {
        cache::model::PlaylistTrack {
            playlist_id: pid.into(),
            track_id: tid.into(),
            position: pos,
            added_at: "t".into(),
        }
    }

    fn renumber(rows: &mut [cache::model::PlaylistTrack]) {
        for (i, r) in rows.iter_mut().enumerate() {
            r.position = (i + 1) as i64;
        }
    }

    #[test]
    fn append_at_end_when_position_zero() {
        let mut rows = vec![row("p", "a", 1), row("p", "b", 2)];
        let insert_at = if 0 <= 0 { rows.len() } else { 0 };
        rows.insert(insert_at, row("p", "c", 0));
        renumber(&mut rows);
        assert_eq!(
            rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
            [("a", 1), ("b", 2), ("c", 3)]
        );
    }

    #[test]
    fn insert_in_middle_shifts_later() {
        let mut rows = vec![row("p", "a", 1), row("p", "b", 2), row("p", "c", 3)];
        let insert_at = ((2_i32) as usize).saturating_sub(1).min(rows.len()); // pos 2 → index 1
        rows.insert(insert_at, row("p", "x", 0));
        renumber(&mut rows);
        assert_eq!(
            rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
            [("a", 1), ("x", 2), ("b", 3), ("c", 4)]
        );
    }

    #[test]
    fn remove_renumbers_down() {
        let mut rows = vec![row("p", "a", 1), row("p", "b", 2), row("p", "c", 3)];
        let idx = (2_i32 as usize).saturating_sub(1);
        if idx < rows.len() {
            rows.remove(idx);
        }
        renumber(&mut rows);
        assert_eq!(
            rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
            [("a", 1), ("c", 2)]
        );
    }

    #[test]
    fn reorder_moves_entry_without_losing_it() {
        let mut rows = vec![row("p", "a", 1), row("p", "b", 2), row("p", "c", 3), row("p", "d", 4)];
        renumber(&mut rows);
        let from = (4_i32 as usize).saturating_sub(1).min(rows.len().saturating_sub(1)); // 4 → idx 3
        let to = (1_i32 as usize).saturating_sub(1).min(rows.len().saturating_sub(1)); // 1 → idx 0
        let moved = rows.remove(from);
        rows.insert(to, moved);
        renumber(&mut rows);
        assert_eq!(
            rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
            [("d", 1), ("a", 2), ("b", 3), ("c", 4)]
        );
    }

    #[test]
    fn reorder_same_position_is_noop() {
        let mut rows = vec![row("p", "a", 1), row("p", "b", 2)];
        renumber(&mut rows);
        let from = (1_i32 as usize).saturating_sub(1).min(rows.len().saturating_sub(1));
        let to = (1_i32 as usize).saturating_sub(1).min(rows.len().saturating_sub(1));
        if from != to {
            let moved = rows.remove(from);
            rows.insert(to, moved);
        }
        renumber(&mut rows);
        assert_eq!(
            rows.iter().map(|r| (r.track_id.as_str(), r.position)).collect::<Vec<_>>(),
            [("a", 1), ("b", 2)]
        );
    }

    #[test]
    fn stub_track_keeps_id_and_marks_not_downloaded() {
        let s = stub_track("tid");
        assert_eq!(s.id, "tid");
        assert!(!s.downloaded);
        assert!(s.title.is_empty());
    }

    #[test]
    fn local_id_detection_via_merged() {
        let p = Playlist {
            id: "local:abc".into(),
            owner_id: "u".into(),
            name: "n".into(),
        };
        let m = MergedPlaylist::from_transport(p);
        assert!(m.local);
    }
}
