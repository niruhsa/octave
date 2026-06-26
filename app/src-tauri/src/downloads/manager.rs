//! Download manager (Phase 6).
//!
//! Fetches track files + album art from the server, stores them under the
//! configured downloads root, and writes the cache rows that make them
//! playable offline (`tracks.local_file_path`, `album_art.local_cover_path`,
//! `downloaded_at`). Bulk album / playlist downloads iterate the per-track
//! path. Deletes remove the file + cache row. Storage accounting sums the
//! cache's recorded file sizes.
//!
//! Resume: each download streams into `<final>.part`; if a `.part` already
//! exists we issue a `Range: bytes=<size>-` and append, then rename
//! atomically on completion. A previously-finished file (cache hit + file
//! present) is a no-op, so re-running a batch download picks up exactly
//! where it left off.
//!
//! Progress is reported via Tauri events on the `download-progress` channel
//! so the UI can render a bar without polling.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use super::artwork;
use super::paths::{
    ensure_dir, episode_extension, episode_file_name, episode_path, track_extension,
    track_file_name, track_path, PART_SUFFIX, SETTING_DOWNLOADS_DIR, SETTING_WIFI_ONLY,
};
use crate::auth::AuthManager;
use crate::cache::repo;
use crate::cache::model as cm;
use crate::error::{AppError, AppResult};
use crate::transport::Credential;

/// Event channel for progress updates.
pub const PROGRESS_EVENT: &str = "download-progress";

/// One progress event. `scope` tells the UI whether this is a single-track
/// update or a batch (album/playlist) aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub scope: ProgressScope,
    pub id: String,
    pub phase: ProgressPhase,
    /// Bytes received so far (progress/done).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<u64>,
    /// Total bytes (when known from Content-Length).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// For batch scope: which track this update is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
    /// For batch scope: 1-based index of the current track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// For batch scope: total tracks in the batch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tracks: Option<u32>,
    /// Error message on the `error` phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressScope {
    Track,
    Batch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressPhase {
    Start,
    Progress,
    Done,
    Error,
}

/// Result of a single track download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackDownloadResult {
    pub track_id: String,
    /// Final on-disk path (set even when skipped, so the UI can show it).
    pub local_path: String,
    pub bytes: u64,
    /// `true` when the file was already present and we did no network I/O.
    pub skipped: bool,
}

/// Result of a bulk (album / playlist) download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchDownloadResult {
    pub id: String,
    pub kind: BatchKind,
    pub total: u32,
    pub succeeded: u32,
    pub skipped: u32,
    pub failed: u32,
    /// Per-track error messages for the failures.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatchKind {
    Album,
    Playlist,
    Podcast,
}

/// Storage usage summary for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageUsage {
    pub bytes: i64,
    pub track_count: i64,
    pub cover_count: i64,
    /// Downloaded podcast episodes + their byte total (additive to `bytes`).
    #[serde(default)]
    pub episode_count: i64,
    #[serde(default)]
    pub episode_bytes: i64,
}

pub struct DownloadManager {
    pool: sqlx::SqlitePool,
    auth: Arc<AuthManager>,
    http: reqwest::Client,
    downloads_root: PathBuf,
    app: AppHandle,
}

impl DownloadManager {
    /// Build from app state. Resolves the downloads root from the settings
    /// override, falling back to `<app_data_dir>/downloads`.
    pub async fn new(
        app: AppHandle,
        pool: sqlx::SqlitePool,
        auth: Arc<AuthManager>,
    ) -> AppResult<Self> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| AppError::Transport(format!("download client: {e}")))?;

        let root = match repo::get_setting(&pool, SETTING_DOWNLOADS_DIR).await? {
            Some(s) if !s.trim().is_empty() => PathBuf::from(s),
            _ => {
                let dir = app
                    .path()
                    .app_data_dir()
                    .map_err(|e| AppError::Internal(format!("app_data_dir: {e}")))?;
                dir.join("downloads")
            }
        };
        ensure_dir(&root).await?;
        Ok(Self {
            pool,
            auth,
            http,
            downloads_root: root,
            app,
        })
    }

    pub fn root(&self) -> &Path {
        &self.downloads_root
    }

    /// Override the downloads root (absolute path). Persists in settings.
    pub async fn set_root(&self, path: &str) -> AppResult<()> {
        let p = PathBuf::from(path);
        if !p.is_absolute() {
            return Err(AppError::Internal("downloads dir must be absolute".into()));
        }
        ensure_dir(&p).await?;
        repo::set_setting(&self.pool, SETTING_DOWNLOADS_DIR, path).await?;
        // We can't reassign `&self.downloads_root`; the next `DownloadManager::new`
        // call picks up the new value. Commands construct a manager per call,
        // so this is fine.
        Ok(())
    }

    /// Wi-Fi-only toggle (mobile). Stored; actual network-type detection
    /// is best-effort and deferred — the UI gates downloads on this flag.
    pub async fn wifi_only(&self) -> AppResult<bool> {
        Ok(repo::get_setting(&self.pool, SETTING_WIFI_ONLY)
            .await?
            .map(|s| s == "true")
            .unwrap_or(false))
    }

    pub async fn set_wifi_only(&self, on: bool) -> AppResult<()> {
        repo::set_setting(&self.pool, SETTING_WIFI_ONLY, if on { "true" } else { "false" }).await
    }

    /// Fetch cover bytes from the server's `GET /albums/:id/cover` endpoint
    /// and write them to `dest`.  Returns `Ok(true)` on success, `Ok(false)`
    /// when the server responds 404 (no cover on the server either).
    async fn fetch_cover_from_server(
        &self,
        album_id: &str,
        dest: &std::path::Path,
    ) -> AppResult<bool> {
        let cred = self.auth.credential().await?;
        let config = self.auth.server_config();
        let url = format!("{}/albums/{}/cover", config.rest_root(), album_id);

        let resp = self
            .http
            .get(&url)
            .header("Authorization", auth_header(&cred))
            .send()
            .await
            .map_err(|e| AppError::Transport(format!("server cover fetch: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                album_id,
                "server cover fetch returned non-2xx"
            );
            return Ok(false);
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Transport(format!("server cover bytes: {e}")))?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(dest, &bytes).await?;
        tracing::info!(
            album_id,
            path = %dest.display(),
            bytes = bytes.len(),
            "downloaded cover from server"
        );
        Ok(true)
    }

    // ----- single track --------------------------------------------------

    pub async fn download_track(&self, track_id: &str) -> AppResult<TrackDownloadResult> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();

        // Already downloaded? Cache hit + file present → no-op.
        if let Some(row) = repo::get_track(&self.pool, track_id).await? {
            if tokio::fs::metadata(&row.local_file_path).await.is_ok() {
                let bytes = row.file_size.unwrap_or(0) as u64;
                self.emit(ProgressEvent {
                    scope: ProgressScope::Track,
                    id: track_id.to_string(),
                    phase: ProgressPhase::Done,
                    received: Some(bytes),
                    total: Some(bytes),
                    track_id: None,
                    index: None,
                    total_tracks: None,
                    message: None,
                });
                return Ok(TrackDownloadResult {
                    track_id: track_id.to_string(),
                    local_path: row.local_file_path,
                    bytes,
                    skipped: true,
                });
            }
        }

        let track = server
            .get_track(&cred, track_id)
            .await?
            .ok_or_else(|| AppError::Transport(format!("track {track_id} not found on server")))?;
        let artist = server
            .get_artist(&cred, &track.artist_id)
            .await?
            .ok_or_else(|| AppError::Transport(format!("artist {} not found", track.artist_id)))?;
        let album = server
            .get_album(&cred, &track.album_id)
            .await?
            .ok_or_else(|| AppError::Transport(format!("album {} not found", track.album_id)))?;

        let ext = track_extension(&track.file_path, &track.codec);
        let file_name = track_file_name(track.track_no, &track.title, &track.id);
        let final_path = track_path(
            &self.downloads_root,
            &artist.name,
            &album.title,
            &file_name,
            &ext,
        );
        ensure_dir(final_path.parent().unwrap_or(&self.downloads_root)).await?;

        let track_url = format!("{}/tracks/{}/stream", server.config().rest_root(), track_id);
        let bytes = self.stream_to_file(&cred, &track_url, track_id, &final_path).await?;

        // Cache rows: artist + album + track + sync_state for each.
        let now = now_iso();
        repo::upsert_artist(
            &self.pool,
            &cm::Artist {
                id: artist.id.clone(),
                name: artist.name.clone(),
                sort_name: artist.sort_name.clone(),
                updated_at: now.clone(),
            },
        )
        .await?;
        repo::upsert_album(
            &self.pool,
            &cm::Album {
                id: album.id.clone(),
                artist_id: album.artist_id.clone(),
                title: album.title.clone(),
                release_year: album.release_year,
                updated_at: now.clone(),
            },
        )
        .await?;
        let local_path = final_path.to_string_lossy().into_owned();
        repo::upsert_track(
            &self.pool,
            &cm::Track {
                id: track.id.clone(),
                album_id: track.album_id.clone(),
                artist_id: track.artist_id.clone(),
                title: track.title.clone(),
                track_no: track.track_no,
                disc_no: track.disc_no,
                duration_ms: track.duration_ms,
                codec: track.codec.clone(),
                bitrate_kbps: track.bitrate_kbps,
                file_size: Some(bytes as i64),
                local_file_path: local_path.clone(),
                metadata_json: track.metadata_json.clone(),
                downloaded_at: now.clone(),
                updated_at: now.clone(),
            },
        )
        .await?;
        stamp(&self.pool, "artist", &artist.id).await?;
        stamp(&self.pool, "album", &album.id).await?;
        stamp(&self.pool, "track", &track.id).await?;

        // Best-effort cover art for the album (skip if already cached).
        if repo::get_album_art(&self.pool, &album.id).await?.is_none() {
            let cover_path = final_path
                .parent()
                .unwrap_or(&self.downloads_root)
                .join("cover.jpg");
            // Try the server's cover endpoint first (the authoritative
            // source — the server fetches + embeds art via the Cover Art
            // Archive automatically during ingest, and stores it under
            // ARTWORK_PATH). Fall back to the client-side CAA lookup only
            // when the server doesn't have a cover or is unreachable.
            let fetched = if album.cover_path.is_some() {
                // Server has a cover — download from the server.
                match self.fetch_cover_from_server(&album.id, &cover_path).await {
                    Ok(true) => true,
                    Ok(false) => {
                        tracing::warn!(
                            album = %album.id,
                            "server has cover_path but cover endpoint returned nothing"
                        );
                        false
                    }
                    Err(e) => {
                        tracing::warn!(
                            err = %e, album = %album.id,
                            "server cover fetch failed; falling back to CAA"
                        );
                        false
                    }
                }
            } else {
                false
            };
            let fetched = if fetched {
                true
            } else {
                // Fall back to the client-side CAA lookup.
                match artwork::fetch_cover(&self.http, &artist.name, &album.title, &cover_path).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            err = %e, album = %album.id,
                            "CAA cover fetch failed; track still downloaded"
                        );
                        false
                    }
                }
            };
            if fetched {
                repo::upsert_album_art(
                    &self.pool,
                    &cm::AlbumArt {
                        album_id: album.id.clone(),
                        local_cover_path: cover_path.to_string_lossy().into_owned(),
                        fetched_at: now.clone(),
                    },
                )
                .await?;
                stamp(&self.pool, "album_art", &album.id).await?;
            } else {
                tracing::debug!(album = %album.id, "no cover art available");
            }
        }

        self.emit(ProgressEvent {
            scope: ProgressScope::Track,
            id: track_id.to_string(),
            phase: ProgressPhase::Done,
            received: Some(bytes),
            total: Some(bytes),
            track_id: None,
            index: None,
            total_tracks: None,
            message: None,
        });

        Ok(TrackDownloadResult {
            track_id: track_id.to_string(),
            local_path,
            bytes,
            skipped: false,
        })
    }

    /// Stream a server byte-range endpoint (`…/tracks/{id}/stream` or
    /// `…/podcasts/episodes/{id}/stream`) into `final_path.part` (resuming from
    /// any existing partial), then rename to `final_path`. Returns the total
    /// file size. `progress_id` is the id emitted on the track-scope progress
    /// events (a track id or an episode id).
    async fn stream_to_file(
        &self,
        cred: &Credential,
        url: &str,
        progress_id: &str,
        final_path: &Path,
    ) -> AppResult<u64> {
        let part_path = final_path.with_extension(format!(
            "{}.{PART_SUFFIX}",
            final_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
        ));

        // Resume: if a .part exists, append from its current length.
        let mut existing: u64 = 0;
        if tokio::fs::try_exists(&part_path).await.unwrap_or(false) {
            existing = tokio::fs::metadata(&part_path).await.map(|m| m.len()).unwrap_or(0);
        }

        let auth = auth_header(cred);
        let mut req = self.http.get(url).header("Authorization", auth);
        if existing > 0 {
            req = req.header("Range", format!("bytes={existing}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Transport(format!("download send: {e}")))?;

        let status = resp.status();
        // 200 → server ignored the range (or none sent): restart from 0.
        // 206 → resume honoured; append.
        // Anything else → error.
        if !status.is_success() {
            return Err(AppError::Transport(format!("download HTTP {status}")));
        }
        let appending = status == reqwest::StatusCode::PARTIAL_CONTENT && existing > 0;

        let total = if appending {
            // Content-Range: bytes start-end/total → total
            resp.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split('/').nth(1))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(existing)
        } else {
            // Fresh start: drop any stale .part.
            if existing > 0 {
                let _ = tokio::fs::remove_file(&part_path).await;
            }
            existing = 0;
            resp.content_length().unwrap_or(0)
        };

        self.emit(ProgressEvent {
            scope: ProgressScope::Track,
            id: progress_id.to_string(),
            phase: ProgressPhase::Start,
            received: Some(existing),
            total: if total > 0 { Some(total) } else { None },
            track_id: None,
            index: None,
            total_tracks: None,
            message: None,
        });

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(appending)
            .truncate(!appending)
            .open(&part_path)
            .await
            .map_err(|e| AppError::Internal(format!("open part: {e}")))?;

        let mut received = existing;
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut last_emit = std::time::Instant::now();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::Transport(format!("download chunk: {e}")))?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            // Throttle progress events to ~10/s so we don't flood the UI.
            if last_emit.elapsed() > std::time::Duration::from_millis(100) {
                self.emit(ProgressEvent {
                    scope: ProgressScope::Track,
                    id: progress_id.to_string(),
                    phase: ProgressPhase::Progress,
                    received: Some(received),
                    total: if total > 0 { Some(total) } else { None },
                    track_id: None,
                    index: None,
                    total_tracks: None,
                    message: None,
                });
                last_emit = std::time::Instant::now();
            }
        }
        file.flush().await?;
        drop(file);

        // Atomic-ish rename to the final path.
        tokio::fs::rename(&part_path, final_path)
            .await
            .map_err(|e| AppError::Internal(format!("finalize rename: {e}")))?;

        // If we never learned the total from headers, the size on disk is
        // the truth.
        let final_size = if total > 0 {
            total
        } else {
            tokio::fs::metadata(final_path).await.map(|m| m.len()).unwrap_or(received)
        };
        Ok(final_size)
    }

    // ----- batch: album --------------------------------------------------

    pub async fn download_album(&self, album_id: &str) -> AppResult<BatchDownloadResult> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();
        // Verify the album exists so a bad id fails fast with a clear error
        // before we kick off N track downloads.
        if server.get_album(&cred, album_id).await?.is_none() {
            return Err(AppError::Transport(format!("album {album_id} not found")));
        }
        let tracks = server.list_tracks_by_album(&cred, album_id).await?;
        self.run_batch(album_id, BatchKind::Album, tracks.into_iter().map(|t| t.id).collect())
            .await
    }

    // ----- batch: playlist -----------------------------------------------

    pub async fn download_playlist(&self, playlist_id: &str) -> AppResult<BatchDownloadResult> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();
        let view = server
            .get_playlist(&cred, playlist_id)
            .await?
            .ok_or_else(|| AppError::Transport(format!("playlist {playlist_id} not found")))?;
        // Dedupe track ids preserving first-seen order.
        let mut ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for t in view.tracks {
            if seen.insert(t.track_id.clone()) {
                ids.push(t.track_id);
            }
        }
        self.run_batch(playlist_id, BatchKind::Playlist, ids).await
    }

    // ----- podcasts ------------------------------------------------------

    /// Download one episode for offline use. Triggers the server to fetch the
    /// enclosure to its disk (the authoritative cached copy), then streams the
    /// server's episode endpoint into the downloads root — the **same**
    /// resumable path as a track. Idempotent: an already-downloaded episode
    /// (cache hit + file present) is a no-op.
    pub async fn download_episode(&self, episode_id: &str) -> AppResult<TrackDownloadResult> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();

        // Already downloaded? Cache hit + file present → no-op.
        if let Some(row) = repo::get_episode(&self.pool, episode_id).await? {
            if let Some(lp) = &row.local_file_path {
                if tokio::fs::metadata(lp).await.is_ok() {
                    let bytes = row.file_size.unwrap_or(0) as u64;
                    self.emit(ProgressEvent {
                        scope: ProgressScope::Track,
                        id: episode_id.to_string(),
                        phase: ProgressPhase::Done,
                        received: Some(bytes),
                        total: Some(bytes),
                        track_id: None,
                        index: None,
                        total_tracks: None,
                        message: None,
                    });
                    return Ok(TrackDownloadResult {
                        track_id: episode_id.to_string(),
                        local_path: lp.clone(),
                        bytes,
                        skipped: true,
                    });
                }
            }
        }

        // Ask the server to fetch the enclosure to its disk, then stream from it
        // (so every client shares the one cached copy). Returns the episode row.
        let ep = server.download_episode(&cred, episode_id).await?;
        let podcast = server.get_podcast(&cred, &ep.podcast_id).await?;

        let ext = episode_extension(&ep.enclosure_url, ep.codec.as_deref());
        let file_name = episode_file_name(ep.episode_no, &ep.title, &ep.id);
        let final_path = episode_path(&self.downloads_root, &podcast.title, &file_name, &ext);
        ensure_dir(final_path.parent().unwrap_or(&self.downloads_root)).await?;

        let url = format!(
            "{}/podcasts/episodes/{}/stream",
            server.config().rest_root(),
            episode_id
        );
        let bytes = self.stream_to_file(&cred, &url, episode_id, &final_path).await?;

        let now = now_iso();
        let local_path = final_path.to_string_lossy().into_owned();

        // Cache the show (preserve the subscribed flag if already cached).
        let subscribed = repo::get_podcast(&self.pool, &podcast.id)
            .await?
            .map(|p| p.subscribed)
            .unwrap_or(0);
        repo::upsert_podcast(
            &self.pool,
            &cm::Podcast {
                id: podcast.id.clone(),
                feed_url: podcast.feed_url.clone(),
                title: podcast.title.clone(),
                author: podcast.author.clone(),
                description: podcast.description.clone(),
                image_url: podcast.image_url.clone(),
                language: podcast.language.clone(),
                categories: serde_json::to_string(&podcast.categories)
                    .unwrap_or_else(|_| "[]".into()),
                subscribed,
                updated_at: now.clone(),
            },
        )
        .await?;
        repo::upsert_episode(
            &self.pool,
            &cm::PodcastEpisode {
                id: ep.id.clone(),
                podcast_id: ep.podcast_id.clone(),
                guid: ep.guid.clone(),
                title: ep.title.clone(),
                description: ep.description.clone(),
                enclosure_url: ep.enclosure_url.clone(),
                episode_no: ep.episode_no,
                season_no: ep.season_no,
                duration_ms: ep.duration_ms,
                codec: ep.codec.clone(),
                bitrate_kbps: ep.bitrate_kbps,
                file_size: Some(bytes as i64),
                local_file_path: Some(local_path.clone()),
                image_path: None,
                published_at: ep.published_at.clone(),
                metadata_json: "{}".to_string(),
                downloaded_at: Some(now.clone()),
                updated_at: now.clone(),
            },
        )
        .await?;
        stamp(&self.pool, "podcast", &podcast.id).await?;
        stamp(&self.pool, "podcast_episode", &ep.id).await?;

        self.emit(ProgressEvent {
            scope: ProgressScope::Track,
            id: episode_id.to_string(),
            phase: ProgressPhase::Done,
            received: Some(bytes),
            total: Some(bytes),
            track_id: None,
            index: None,
            total_tracks: None,
            message: None,
        });

        Ok(TrackDownloadResult {
            track_id: episode_id.to_string(),
            local_path,
            bytes,
            skipped: false,
        })
    }

    /// Download the newest `newest_n` not-yet-downloaded episodes of a show
    /// (feeds are large, so we don't grab the whole back-catalog). Reuses the
    /// batch machinery (progress events + Android foreground-service notif).
    pub async fn download_podcast(
        &self,
        podcast_id: &str,
        newest_n: Option<u32>,
    ) -> AppResult<BatchDownloadResult> {
        let cred = self.auth.credential().await?;
        let server = self.auth.server();
        // Newest-first page; the server already orders by published date.
        let limit = newest_n.unwrap_or(10).clamp(1, 200) as i32;
        let episodes = server.list_episodes(&cred, podcast_id, limit, 0).await?;
        let ids: Vec<String> = episodes.into_iter().map(|e| e.id).collect();
        self.run_batch(podcast_id, BatchKind::Podcast, ids).await
    }

    async fn run_batch(
        &self,
        batch_id: &str,
        kind: BatchKind,
        track_ids: Vec<String>,
    ) -> AppResult<BatchDownloadResult> {
        let total = track_ids.len() as u32;
        // Foreground-service notification title (Android; no-op on desktop).
        let title = match kind {
            BatchKind::Album => "Downloading album",
            BatchKind::Playlist => "Downloading playlist",
            BatchKind::Podcast => "Downloading podcast",
        };
        self.emit(ProgressEvent {
            scope: ProgressScope::Batch,
            id: batch_id.to_string(),
            phase: ProgressPhase::Start,
            received: None,
            total: None,
            track_id: None,
            index: None,
            total_tracks: Some(total),
            message: None,
        });

        let mut succeeded = 0u32;
        let mut skipped = 0u32;
        let mut failed = 0u32;
        let mut errors = Vec::new();
        for (i, tid) in track_ids.iter().enumerate() {
            let idx = (i + 1) as u32;
            // Update the foreground-service notification with determinate
            // progress by completed-track count (Android; no-op on desktop).
            let pct = if total > 0 {
                ((i as f64 / total as f64) * 100.0) as i32
            } else {
                0
            };
            let unit = match kind {
                BatchKind::Podcast => "Episode",
                _ => "Track",
            };
            crate::download_session::update(
                &self.app,
                title,
                &format!("{unit} {idx} of {total}"),
                pct,
            );
            let outcome = match kind {
                BatchKind::Podcast => self.download_episode(tid).await,
                _ => self.download_track(tid).await,
            };
            match outcome {
                Ok(r) => {
                    if r.skipped {
                        skipped += 1;
                    } else {
                        succeeded += 1;
                    }
                }
                Err(e) => {
                    failed += 1;
                    let msg = format!("track {tid}: {e}");
                    tracing::warn!(err = %e, "batch track failed");
                    errors.push(msg);
                    self.emit(ProgressEvent {
                        scope: ProgressScope::Batch,
                        id: batch_id.to_string(),
                        phase: ProgressPhase::Error,
                        received: None,
                        total: None,
                        track_id: Some(tid.clone()),
                        index: Some(idx),
                        total_tracks: Some(total),
                        message: Some(e.to_string()),
                    });
                }
            }
            self.emit(ProgressEvent {
                scope: ProgressScope::Batch,
                id: batch_id.to_string(),
                phase: ProgressPhase::Progress,
                received: Some((i + 1) as u64),
                total: Some(total as u64),
                track_id: Some(tid.clone()),
                index: Some(idx),
                total_tracks: Some(total),
                message: None,
            });
        }

        self.emit(ProgressEvent {
            scope: ProgressScope::Batch,
            id: batch_id.to_string(),
            phase: ProgressPhase::Done,
            received: Some(total as u64),
            total: Some(total as u64),
            track_id: None,
            index: None,
            total_tracks: Some(total),
            message: None,
        });

        Ok(BatchDownloadResult {
            id: batch_id.to_string(),
            kind,
            total,
            succeeded,
            skipped,
            failed,
            errors,
        })
    }

    // ----- delete --------------------------------------------------------

    /// Remove a downloaded track: delete the file + the cache row + its
    /// sync_state. If the album has no downloaded tracks left, also drop
    /// the cover file + `album_art` row. Best-effort cleanup of now-empty
    /// artist/album directories.
    pub async fn delete_track(&self, track_id: &str) -> AppResult<()> {
        let row = repo::get_track(&self.pool, track_id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("track {track_id} not in cache")))?;
        let album_id = row.album_id.clone();
        let path = PathBuf::from(&row.local_file_path);

        // Remove the file (ignore "not found").
        let _ = tokio::fs::remove_file(&path).await;

        repo::delete_track(&self.pool, track_id).await?;
        repo::delete_sync_state(&self.pool, "track", track_id).await?;

        // Cover cleanup: only when the album has zero downloads left.
        if repo::count_downloaded_tracks_for_album(&self.pool, &album_id).await? == 0 {
            if let Some(art) = repo::get_album_art(&self.pool, &album_id).await? {
                let _ = tokio::fs::remove_file(&art.local_cover_path).await;
            }
            repo::delete_album_art(&self.pool, &album_id).await?;
            repo::delete_sync_state(&self.pool, "album_art", &album_id).await?;
            // Also drop the album row itself — nothing of it is downloaded.
            repo::delete_album(&self.pool, &album_id).await?;
            repo::delete_sync_state(&self.pool, "album", &album_id).await?;
            // Prune empty artist dir + artist row if no albums remain.
            if let Some(parent) = path.parent().and_then(|p| p.parent()) {
                let _ = tokio::fs::remove_dir(parent).await;
            }
        }
        // Always try to remove the now-empty album dir.
        if let Some(album_dir) = path.parent() {
            let _ = tokio::fs::remove_dir(album_dir).await;
        }
        Ok(())
    }

    /// Remove a downloaded episode: delete the file + clear the cache row's
    /// local path (keeping the metadata for the show view) + its sync_state.
    /// When the show has no downloaded episodes left and the user isn't
    /// subscribed, drop the show row too; best-effort prune of the now-empty
    /// show directory.
    pub async fn delete_episode(&self, episode_id: &str) -> AppResult<()> {
        let row = repo::get_episode(&self.pool, episode_id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("episode {episode_id} not in cache")))?;
        let podcast_id = row.podcast_id.clone();
        let path = row.local_file_path.as_ref().map(PathBuf::from);

        if let Some(p) = &path {
            let _ = tokio::fs::remove_file(p).await;
        }
        repo::delete_episode(&self.pool, episode_id).await?;
        repo::delete_sync_state(&self.pool, "podcast_episode", episode_id).await?;

        // If nothing of this show is downloaded any more and it isn't a
        // subscription, drop the show + prune its (now-empty) directory.
        if repo::count_downloaded_episodes_for_podcast(&self.pool, &podcast_id).await? == 0 {
            let still_subscribed = repo::get_podcast(&self.pool, &podcast_id)
                .await?
                .map(|p| p.subscribed != 0)
                .unwrap_or(false);
            if !still_subscribed {
                repo::delete_podcast(&self.pool, &podcast_id).await?;
                repo::delete_sync_state(&self.pool, "podcast", &podcast_id).await?;
            }
            if let Some(show_dir) = path.as_ref().and_then(|p| p.parent()) {
                let _ = tokio::fs::remove_dir(show_dir).await;
            }
        }
        Ok(())
    }

    // ----- storage accounting -------------------------------------------

    pub async fn storage_usage(&self) -> AppResult<StorageUsage> {
        let track_bytes = repo::downloaded_bytes(&self.pool).await?;
        let track_count = repo::count_downloaded_tracks(&self.pool).await?;
        let cover_count = repo::downloaded_cover_count(&self.pool).await?;
        let episode_bytes = repo::downloaded_episode_bytes(&self.pool).await?;
        let episode_count = repo::count_downloaded_episodes(&self.pool).await?;
        Ok(StorageUsage {
            // `bytes` is the grand total so existing UI keeps working.
            bytes: track_bytes + episode_bytes,
            track_count,
            cover_count,
            episode_count,
            episode_bytes,
        })
    }

    // ----- helpers -------------------------------------------------------

    fn emit(&self, ev: ProgressEvent) {
        if let Err(e) = self.app.emit(PROGRESS_EVENT, &ev) {
            tracing::warn!(err = %e, "failed to emit download progress event");
        }
    }
}

fn auth_header(cred: &Credential) -> String {
    match cred {
        Credential::SecretKey(k) => format!("SecretKey {k}"),
        Credential::Bearer(t) => format!("Bearer {t}"),
    }
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Stamp a sync_state row so the next reconcile knows we just touched this
/// entity locally (content hash left empty — the engine overwrites it on
/// the next pull).
async fn stamp(pool: &sqlx::SqlitePool, entity_type: &str, id: &str) -> AppResult<()> {
    repo::upsert_sync_state(
        pool,
        &cm::SyncState {
            entity_type: entity_type.to_string(),
            entity_id: id.to_string(),
            server_version: None,
            server_etag: None,
            last_synced_at: now_iso(),
        },
    )
    .await
}


