//! The reconciliation engine. See [`super`] for the high-level contract.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::ops::{is_local_id, PendingOpKind};
use crate::auth::AuthManager;
use crate::cache::model as cm;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::transport::{Credential, ServerClient};

/// Summary of one `sync_now` run, surfaced to the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncReport {
    /// Outbox ops successfully replayed to the server.
    pub ops_pushed: u32,
    /// Ops the server rejected on their merits (conflicts; dropped).
    pub ops_conflicted: u32,
    /// Ops left queued (transport failure; will retry).
    pub ops_deferred: u32,
    /// Cached rows updated from the server.
    pub entities_updated: u32,
    /// Cached rows pruned because the server no longer has them.
    pub entities_pruned: u32,
    /// Downloaded tracks pruned because their local file vanished.
    pub files_missing: u32,
    /// Human-readable conflict descriptions for the UI to surface.
    pub conflicts: Vec<String>,
}

pub struct SyncEngine {
    pool: SqlitePool,
    auth: Arc<AuthManager>,
}

impl SyncEngine {
    pub fn new(pool: SqlitePool, auth: Arc<AuthManager>) -> Self {
        Self { pool, auth }
    }

    /// Run a full sync: push outbox → pull/reconcile → prune. Requires a
    /// live credential; returns `AuthNotConfigured` when anonymous so the
    /// caller can skip silently.
    pub async fn sync_now(&self) -> AppResult<SyncReport> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();
        let mut report = SyncReport::default();

        // 1. Push first so local edits land before we pull server truth
        //    (avoids a pull clobbering an unsynced local change).
        self.push_outbox(server, &cred, &mut report).await?;

        // 2. Pull + reconcile every cached entity kind.
        self.pull_artists(server, &cred, &mut report).await?;
        self.pull_albums(server, &cred, &mut report).await?;
        self.pull_tracks(server, &cred, &mut report).await?;
        self.pull_playlists(server, &cred, &mut report).await?;
        self.pull_podcasts(server, &cred, &mut report).await?;
        self.pull_podcast_episodes(server, &cred, &mut report).await?;

        // 3. Prune downloads whose files disappeared.
        self.prune_missing_files(&mut report).await?;
        self.prune_missing_episode_files(&mut report).await?;

        tracing::info!(?report, "sync complete");
        Ok(report)
    }

    // ----- push ----------------------------------------------------------

    async fn push_outbox(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        let ops = repo::list_pending_ops(&self.pool).await?;
        for row in ops {
            let kind = match PendingOpKind::from_payload_json(&row.payload_json) {
                Ok(k) => k,
                Err(e) => {
                    // Undecodable op — drop it, it can never succeed.
                    tracing::warn!(id = row.id, err = %e, "dropping corrupt pending op");
                    repo::delete_pending_op(&self.pool, row.id).await?;
                    continue;
                }
            };

            match self.apply_op(server, cred, &kind).await {
                Ok(remap) => {
                    // On a create, rewrite the temp id everywhere before we
                    // delete the op (later ops + cache rows).
                    if let Some((old_id, new_id)) = remap {
                        self.remap_local_id(&old_id, &new_id).await?;
                    }
                    repo::delete_pending_op(&self.pool, row.id).await?;
                    report.ops_pushed += 1;
                }
                Err(e) if is_transport_error(&e) => {
                    // Server unreachable mid-run: stop, keep this + the rest
                    // queued, report them as deferred.
                    repo::mark_op_failed(&self.pool, row.id, &e.to_string()).await?;
                    let remaining = repo::count_pending_ops(&self.pool).await?;
                    report.ops_deferred = remaining as u32;
                    return Ok(());
                }
                Err(e) => {
                    // Server rejected the op on its merits → conflict. Drop
                    // it; the subsequent pull reconciles local state.
                    tracing::warn!(id = row.id, err = %e, "pending op rejected; dropping");
                    repo::delete_pending_op(&self.pool, row.id).await?;
                    report.ops_conflicted += 1;
                    report.conflicts.push(format!("{}: {e}", kind.op_type()));
                }
            }
        }
        Ok(())
    }

    /// Execute one op against the server. Returns `Some((old, new))` when a
    /// create resolved a temp id that callers must remap.
    async fn apply_op(
        &self,
        server: &ServerClient,
        cred: &Credential,
        kind: &PendingOpKind,
    ) -> AppResult<Option<(String, String)>> {
        match kind {
            PendingOpKind::PlaylistCreate { local_id, name } => {
                let created = server.create_playlist(cred, name).await?;
                Ok(Some((local_id.clone(), created.id)))
            }
            PendingOpKind::PlaylistRename { playlist_id, name } => {
                guard_resolved(playlist_id)?;
                server.rename_playlist(cred, playlist_id, name).await?;
                Ok(None)
            }
            PendingOpKind::PlaylistDelete { playlist_id } => {
                guard_resolved(playlist_id)?;
                server.delete_playlist(cred, playlist_id).await?;
                Ok(None)
            }
            PendingOpKind::PlaylistAddTrack {
                playlist_id,
                track_id,
                position,
            } => {
                guard_resolved(playlist_id)?;
                server
                    .add_playlist_track(cred, playlist_id, track_id, *position)
                    .await?;
                Ok(None)
            }
            PendingOpKind::PlaylistRemoveTrack {
                playlist_id,
                position,
            } => {
                guard_resolved(playlist_id)?;
                server
                    .remove_playlist_track(cred, playlist_id, *position)
                    .await?;
                Ok(None)
            }
            PendingOpKind::PlaylistReorderTrack {
                playlist_id,
                from_position,
                to_position,
            } => {
                guard_resolved(playlist_id)?;
                server
                    .reorder_playlist_track(cred, playlist_id, *from_position, *to_position)
                    .await?;
                Ok(None)
            }
        }
    }

    /// Rewrite a temp playlist id to the server-issued id across queued ops
    /// and cache rows. Runs after a `playlist.create` succeeds.
    async fn remap_local_id(&self, old_id: &str, new_id: &str) -> AppResult<()> {
        // Queued ops still referencing the temp id.
        for row in repo::list_pending_ops(&self.pool).await? {
            if let Ok(mut kind) = PendingOpKind::from_payload_json(&row.payload_json) {
                if kind.references_playlist(old_id) {
                    kind.remap_playlist(old_id, new_id);
                    let json = kind.to_payload_json()?;
                    sqlx::query("UPDATE pending_ops SET payload_json = ?2 WHERE id = ?1")
                        .bind(row.id)
                        .bind(json)
                        .execute(&self.pool)
                        .await?;
                }
            }
        }
        // Cache rows: a straight `UPDATE playlists SET id` would violate the
        // playlist_tracks FK (children still point at the old id). Do it in
        // one transaction as insert-new → repoint-children → delete-old.
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO playlists (id, owner_id, name, updated_at)
             SELECT ?2, owner_id, name, updated_at FROM playlists WHERE id = ?1",
        )
        .bind(old_id)
        .bind(new_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE playlist_tracks SET playlist_id = ?2 WHERE playlist_id = ?1")
            .bind(old_id)
            .bind(new_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM playlists WHERE id = ?1")
            .bind(old_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // ----- pull / reconcile ---------------------------------------------

    /// Delete a downloaded track file from disk, best-effort. Logs the
    /// attempt — missing files are not considered errors since the prune
    /// step already covers that case.
    async fn delete_track_file(path: &str) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => tracing::info!(path, "deleted pruned track file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path, "pruned track file already gone");
            }
            Err(e) => tracing::warn!(path, err = %e, "failed to delete pruned track file"),
        }
    }

    async fn pull_artists(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        for row in repo::list_artists(&self.pool).await? {
            match server.get_artist(cred, &row.id).await? {
                Some(srv) => {
                    let hash = hash_fields(&[
                        &srv.name,
                        srv.sort_name.as_deref().unwrap_or(""),
                        &srv.storage_bytes.to_string(),
                    ]);
                    if self.changed("artist", &row.id, &hash).await? {
                        let updated = cm::Artist {
                            id: srv.id,
                            name: srv.name,
                            sort_name: srv.sort_name,
                            storage_bytes: srv.storage_bytes,
                            updated_at: now_iso(),
                        };
                        repo::upsert_artist(&self.pool, &updated).await?;
                        self.stamp("artist", &updated.id, &hash).await?;
                        report.entities_updated += 1;
                    }
                }
                None => {
                    // Delete all downloaded track files + album covers
                    // under this artist before cascading DB rows.
                    let tracks = repo::list_tracks_by_artist(&self.pool, &row.id).await?;
                    for t in &tracks {
                        Self::delete_track_file(&t.local_file_path).await;
                    }
                    let albums = repo::list_albums_by_artist(&self.pool, &row.id).await?;
                    for a in &albums {
                        if let Some(art) = repo::get_album_art(&self.pool, &a.id).await? {
                            Self::delete_track_file(&art.local_cover_path).await;
                        }
                    }
                    repo::delete_artist(&self.pool, &row.id).await?;
                    repo::delete_sync_state(&self.pool, "artist", &row.id).await?;
                    // Clean up per-track and per-album sync states that
                    // `delete_artist` cascaded.
                    for t in &tracks {
                        repo::delete_sync_state(&self.pool, "track", &t.id).await?;
                    }
                    for a in &albums {
                        repo::delete_sync_state(&self.pool, "album", &a.id).await?;
                    }
                    report.entities_pruned += 1;
                }
            }
        }
        Ok(())
    }

    async fn pull_albums(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        // The cache only stores albums under known artists; gather them by
        // walking artists (bounded — offline cache, not full catalog).
        let artists = repo::list_artists(&self.pool).await?;
        for artist in artists {
            for row in repo::list_albums_by_artist(&self.pool, &artist.id).await? {
                match server.get_album(cred, &row.id).await? {
                    Some(srv) => {
                        let hash = hash_fields(&[
                            &srv.artist_id,
                            &srv.title,
                            &srv.release_year.map(|y| y.to_string()).unwrap_or_default(),
                            &srv.storage_bytes.to_string(),
                        ]);
                        if self.changed("album", &row.id, &hash).await? {
                            let updated = cm::Album {
                                id: srv.id,
                                artist_id: srv.artist_id,
                                title: srv.title,
                                release_year: srv.release_year,
                                storage_bytes: srv.storage_bytes,
                                updated_at: now_iso(),
                            };
                            repo::upsert_album(&self.pool, &updated).await?;
                            self.stamp("album", &updated.id, &hash).await?;
                            report.entities_updated += 1;
                        }
                    }
                    None => {
                        // Delete all downloaded track files + the album's
                        // cover art file before cascading the DB rows.
                        let tracks = repo::list_tracks_by_album(&self.pool, &row.id).await?;
                        for t in &tracks {
                            Self::delete_track_file(&t.local_file_path).await;
                        }
                        if let Some(art) = repo::get_album_art(&self.pool, &row.id).await? {
                            Self::delete_track_file(&art.local_cover_path).await;
                        }
                        repo::delete_album(&self.pool, &row.id).await?;
                        repo::delete_sync_state(&self.pool, "album", &row.id).await?;
                        // Also clear the per-track sync states that
                        // `delete_album` cascaded.
                        for t in &tracks {
                            repo::delete_sync_state(&self.pool, "track", &t.id).await?;
                        }
                        report.entities_pruned += 1;
                    }
                }
            }
        }
        Ok(())
    }

    async fn pull_tracks(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        for row in repo::list_downloaded_tracks(&self.pool).await? {
            match server.get_track(cred, &row.id).await? {
                Some(srv) => {
                    // Metadata fields only — never touch `local_file_path`
                    // / `downloaded_at`, which are client-owned.
                    let hash = hash_fields(&[
                        &srv.album_id,
                        &srv.artist_id,
                        &srv.title,
                        &srv.track_no.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.disc_no.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.duration_ms.to_string(),
                        &srv.codec,
                        &srv.sample_rate_hz.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.bit_depth.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.channels.map(|n| n.to_string()).unwrap_or_default(),
                        // Loudness (Phase 16) — so a track measured *after* it was
                        // downloaded re-syncs and normalizes offline.
                        &srv.loudness_lufs.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.loudness_peak.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.album_loudness_lufs.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.metadata_json,
                    ]);
                    if self.changed("track", &row.id, &hash).await? {
                        let updated = cm::Track {
                            id: srv.id,
                            album_id: srv.album_id,
                            artist_id: srv.artist_id,
                            title: srv.title,
                            track_no: srv.track_no,
                            disc_no: srv.disc_no,
                            duration_ms: srv.duration_ms,
                            codec: srv.codec,
                            bitrate_kbps: srv.bitrate_kbps,
                            file_size: srv.file_size,
                            sample_rate_hz: srv.sample_rate_hz,
                            bit_depth: srv.bit_depth,
                            channels: srv.channels,
                            loudness_lufs: srv.loudness_lufs,
                            loudness_peak: srv.loudness_peak,
                            album_loudness_lufs: srv.album_loudness_lufs,
                            // preserve client-owned fields
                            local_file_path: row.local_file_path.clone(),
                            metadata_json: srv.metadata_json,
                            downloaded_at: row.downloaded_at.clone(),
                            updated_at: now_iso(),
                        };
                        repo::upsert_track(&self.pool, &updated).await?;
                        self.stamp("track", &updated.id, &hash).await?;
                        report.entities_updated += 1;
                    }
                }
                None => {
                    Self::delete_track_file(&row.local_file_path).await;
                    repo::delete_track(&self.pool, &row.id).await?;
                    repo::delete_sync_state(&self.pool, "track", &row.id).await?;
                    report.entities_pruned += 1;
                }
            }
        }
        Ok(())
    }

    async fn pull_playlists(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        for row in repo::list_playlists(&self.pool).await? {
            // Skip unresolved local playlists — they only exist in the
            // outbox until their create op replays.
            if is_local_id(&row.id) {
                continue;
            }
            match server.get_playlist(cred, &row.id).await? {
                Some(view) => {
                    let p = view.playlist;
                    // Hash playlist name + the ordered track list so reorder
                    // / add / remove all register as a change.
                    let track_sig: String = view
                        .tracks
                        .iter()
                        .map(|t| format!("{}:{}", t.position, t.track_id))
                        .collect::<Vec<_>>()
                        .join(",");
                    let hash = hash_fields(&[&p.owner_id, &p.name, &track_sig]);
                    if self.changed("playlist", &row.id, &hash).await? {
                        let updated = cm::Playlist {
                            id: p.id.clone(),
                            owner_id: p.owner_id,
                            name: p.name,
                            updated_at: now_iso(),
                        };
                        repo::upsert_playlist(&self.pool, &updated).await?;
                        let entries: Vec<cm::PlaylistTrack> = view
                            .tracks
                            .into_iter()
                            .map(|t| cm::PlaylistTrack {
                                playlist_id: p.id.clone(),
                                track_id: t.track_id,
                                position: t.position,
                                added_at: now_iso(),
                            })
                            .collect();
                        repo::replace_playlist_tracks(&self.pool, &p.id, &entries).await?;
                        self.stamp("playlist", &p.id, &hash).await?;
                        report.entities_updated += 1;
                    }
                }
                None => {
                    repo::delete_playlist(&self.pool, &row.id).await?;
                    repo::delete_sync_state(&self.pool, "playlist", &row.id).await?;
                    report.entities_pruned += 1;
                }
            }
        }
        Ok(())
    }

    /// Reconcile cached podcast shows. Metadata-only — `subscribed` is
    /// client-owned. A fetch error (offline / since-deleted) leaves the row
    /// in place rather than risk pruning on a transient failure (a deleted
    /// subscribed show simply stops appearing in the online `list_subscriptions`).
    async fn pull_podcasts(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        for row in repo::list_all_podcasts(&self.pool).await? {
            match server.get_podcast(cred, &row.id).await {
                Ok(srv) => {
                    let cats =
                        serde_json::to_string(&srv.categories).unwrap_or_else(|_| "[]".into());
                    let hash = hash_fields(&[
                        &srv.feed_url,
                        &srv.title,
                        srv.author.as_deref().unwrap_or(""),
                        srv.description.as_deref().unwrap_or(""),
                        srv.image_url.as_deref().unwrap_or(""),
                        srv.language.as_deref().unwrap_or(""),
                        &cats,
                        &srv.storage_bytes.to_string(),
                    ]);
                    if self.changed("podcast", &row.id, &hash).await? {
                        let updated = cm::Podcast {
                            id: srv.id,
                            feed_url: srv.feed_url,
                            title: srv.title,
                            author: srv.author,
                            description: srv.description,
                            image_url: srv.image_url,
                            language: srv.language,
                            categories: cats,
                            subscribed: row.subscribed, // client-owned
                            storage_bytes: srv.storage_bytes,
                            updated_at: now_iso(),
                        };
                        repo::upsert_podcast(&self.pool, &updated).await?;
                        self.stamp("podcast", &updated.id, &hash).await?;
                        report.entities_updated += 1;
                    }
                }
                Err(e) => tracing::debug!(podcast = %row.id, err = %e, "podcast reconcile skipped"),
            }
        }
        Ok(())
    }

    /// Reconcile downloaded episodes' metadata. Client-owned fields
    /// (`local_file_path` / `downloaded_at` / `file_size`) are preserved.
    async fn pull_podcast_episodes(
        &self,
        server: &ServerClient,
        cred: &Credential,
        report: &mut SyncReport,
    ) -> AppResult<()> {
        for row in repo::list_downloaded_episodes(&self.pool).await? {
            match server.get_episode(cred, &row.id).await {
                Ok(srv) => {
                    let hash = hash_fields(&[
                        &srv.podcast_id,
                        &srv.title,
                        srv.description.as_deref().unwrap_or(""),
                        &srv.enclosure_url,
                        &srv.episode_no.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.season_no.map(|n| n.to_string()).unwrap_or_default(),
                        &srv.duration_ms.map(|n| n.to_string()).unwrap_or_default(),
                    ]);
                    if self.changed("podcast_episode", &row.id, &hash).await? {
                        let updated = cm::PodcastEpisode {
                            id: srv.id,
                            podcast_id: srv.podcast_id,
                            guid: srv.guid,
                            title: srv.title,
                            description: srv.description,
                            enclosure_url: srv.enclosure_url,
                            episode_no: srv.episode_no,
                            season_no: srv.season_no,
                            duration_ms: srv.duration_ms.or(row.duration_ms),
                            codec: srv.codec.or(row.codec.clone()),
                            bitrate_kbps: srv.bitrate_kbps.or(row.bitrate_kbps),
                            // client-owned (reflect the on-disk file)
                            file_size: row.file_size,
                            local_file_path: row.local_file_path.clone(),
                            image_path: row.image_path.clone(),
                            published_at: srv.published_at,
                            metadata_json: row.metadata_json.clone(),
                            downloaded_at: row.downloaded_at.clone(),
                            updated_at: now_iso(),
                        };
                        repo::upsert_episode(&self.pool, &updated).await?;
                        self.stamp("podcast_episode", &updated.id, &hash).await?;
                        report.entities_updated += 1;
                    }
                }
                Err(e) => tracing::debug!(episode = %row.id, err = %e, "episode reconcile skipped"),
            }
        }
        Ok(())
    }

    // ----- prune ---------------------------------------------------------

    async fn prune_missing_files(&self, report: &mut SyncReport) -> AppResult<()> {
        for row in repo::list_downloaded_tracks(&self.pool).await? {
            let exists = tokio::fs::metadata(&row.local_file_path).await.is_ok();
            if !exists {
                tracing::warn!(track = %row.id, path = %row.local_file_path, "downloaded file missing; pruning row");
                repo::delete_track(&self.pool, &row.id).await?;
                repo::delete_sync_state(&self.pool, "track", &row.id).await?;
                report.files_missing += 1;
            }
        }
        Ok(())
    }

    /// Prune downloaded-episode rows whose local file vanished from disk.
    async fn prune_missing_episode_files(&self, report: &mut SyncReport) -> AppResult<()> {
        for row in repo::list_downloaded_episodes(&self.pool).await? {
            let Some(lp) = row.local_file_path.as_deref() else {
                continue;
            };
            if tokio::fs::metadata(lp).await.is_err() {
                tracing::warn!(episode = %row.id, path = %lp, "downloaded episode file missing; pruning row");
                repo::delete_episode(&self.pool, &row.id).await?;
                repo::delete_sync_state(&self.pool, "podcast_episode", &row.id).await?;
                report.files_missing += 1;
            }
        }
        Ok(())
    }

    // ----- versioning helpers -------------------------------------------

    /// Has the server row's content hash changed since we last stamped it?
    /// A missing sync_state row counts as "changed" (first sync).
    async fn changed(&self, entity_type: &str, id: &str, hash: &str) -> AppResult<bool> {
        let existing = repo::get_sync_state(&self.pool, entity_type, id).await?;
        Ok(match existing {
            Some(s) => s.server_etag.as_deref() != Some(hash),
            None => true,
        })
    }

    /// Record the server content hash so the next sync can skip unchanged
    /// rows.
    async fn stamp(&self, entity_type: &str, id: &str, hash: &str) -> AppResult<()> {
        repo::upsert_sync_state(
            &self.pool,
            &cm::SyncState {
                entity_type: entity_type.to_string(),
                entity_id: id.to_string(),
                server_version: None,
                server_etag: Some(hash.to_string()),
                last_synced_at: now_iso(),
            },
        )
        .await
    }
}

fn guard_resolved(playlist_id: &str) -> AppResult<()> {
    if is_local_id(playlist_id) {
        // A dependent op outran its create — treat as a permanent failure so
        // the engine drops it rather than spinning forever.
        return Err(AppError::Internal(format!(
            "op references unresolved local id {playlist_id}"
        )));
    }
    Ok(())
}

fn is_transport_error(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_))
}

/// Stable content hash of a row's significant fields. Order matters; the
/// caller passes fields in a fixed order. Hex-encoded `DefaultHasher` —
/// not cryptographic, just a cheap change-detector.
fn hash_fields(fields: &[&str]) -> String {
    let mut h = DefaultHasher::new();
    for f in fields {
        f.hash(&mut h);
        0u8.hash(&mut h); // separator so ["a","b"] != ["ab"]
    }
    format!("{:016x}", h.finish())
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_order_sensitive_and_stable() {
        let a = hash_fields(&["x", "y"]);
        let b = hash_fields(&["x", "y"]);
        let c = hash_fields(&["y", "x"]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn hash_distinguishes_field_boundaries() {
        assert_ne!(hash_fields(&["a", "b"]), hash_fields(&["ab", ""]));
    }

    #[test]
    fn guard_rejects_local_id() {
        assert!(guard_resolved("local:abc").is_err());
        assert!(guard_resolved("real-uuid").is_ok());
    }
}
