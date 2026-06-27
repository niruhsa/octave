//! Podcast service: server-backed reads when online, cache-only fallback when
//! offline. Mirrors `library::service` — each result carries its offline state.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqlx::SqlitePool;

use super::merged::{MergedEpisode, MergedPodcast};
use crate::auth::AuthManager;
use crate::cache::model as cache_model;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::library::LibraryView;
use crate::transport::{Podcast, PodcastCandidate, PodcastEpisode, RefreshReport};

/// Same "we have no usable server" signal the library service uses.
fn is_offline_signal(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_) | AppError::AuthNotConfigured(_))
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

pub struct PodcastService<'a> {
    pool: &'a SqlitePool,
    auth: Arc<AuthManager>,
}

impl<'a> PodcastService<'a> {
    pub fn new(pool: &'a SqlitePool, auth: Arc<AuthManager>) -> Self {
        Self { pool, auth }
    }

    /// Directory search — online only (the directory lives on the server).
    pub async fn search(&self, term: &str, limit: i64) -> AppResult<Vec<PodcastCandidate>> {
        self.auth.search_podcasts(term, limit.clamp(1, 200) as i32).await
    }

    // ----- subscriptions list --------------------------------------------

    pub async fn list_subscriptions(&self) -> AppResult<LibraryView<MergedPodcast>> {
        match self.try_server_list_subscriptions().await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_subscriptions: server unavailable, serving cache");
                self.list_subscriptions_from_cache().await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list_subscriptions(&self) -> AppResult<LibraryView<MergedPodcast>> {
        let subs = self.auth.list_subscriptions().await?;
        let mut items = Vec::with_capacity(subs.len());
        for p in subs {
            // Cache the show as subscribed so the list renders offline.
            self.cache_upsert_podcast(&p, true).await?;
            let count = repo::count_downloaded_episodes_for_podcast(self.pool, &p.id).await?;
            items.push(MergedPodcast::from_server(p, true, count));
        }
        Ok(LibraryView::server(items))
    }

    async fn list_subscriptions_from_cache(&self) -> AppResult<LibraryView<MergedPodcast>> {
        let rows = repo::list_subscribed_podcasts(self.pool).await?;
        let mut items = Vec::with_capacity(rows.len());
        for p in rows {
            let count = repo::count_downloaded_episodes_for_podcast(self.pool, &p.id).await?;
            items.push(MergedPodcast::from_cache(p, count));
        }
        Ok(LibraryView::cache(items))
    }

    // ----- single show ----------------------------------------------------

    pub async fn get_podcast(&self, id: &str) -> AppResult<MergedPodcast> {
        match self.try_server_get_podcast(id).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "get_podcast: server unavailable, serving cache");
                self.get_podcast_from_cache(id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_get_podcast(&self, id: &str) -> AppResult<MergedPodcast> {
        let p = self.auth.get_podcast(id).await?;
        // Best-effort subscribed flag (a SECRET_KEY session can't own a sub →
        // treat as not-subscribed rather than failing the read).
        let subscribed = self.auth.is_subscribed(id).await.unwrap_or(false);
        self.cache_upsert_podcast(&p, subscribed).await?;
        let count = repo::count_downloaded_episodes_for_podcast(self.pool, id).await?;
        Ok(MergedPodcast::from_server(p, subscribed, count))
    }

    async fn get_podcast_from_cache(&self, id: &str) -> AppResult<MergedPodcast> {
        let p = repo::get_podcast(self.pool, id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("podcast {id} not cached")))?;
        let count = repo::count_downloaded_episodes_for_podcast(self.pool, id).await?;
        Ok(MergedPodcast::from_cache(p, count))
    }

    // ----- episodes -------------------------------------------------------

    pub async fn list_episodes(
        &self,
        podcast_id: &str,
    ) -> AppResult<LibraryView<MergedEpisode>> {
        match self.try_server_list_episodes(podcast_id).await {
            Ok(v) => Ok(v),
            Err(e) if is_offline_signal(&e) => {
                tracing::info!(err = %e, "list_episodes: server unavailable, serving cache");
                self.list_episodes_from_cache(podcast_id).await
            }
            Err(e) => Err(e),
        }
    }

    async fn try_server_list_episodes(
        &self,
        podcast_id: &str,
    ) -> AppResult<LibraryView<MergedEpisode>> {
        // The server caps a single response at 200. Rather than re-pull the
        // whole feed on every open, page newest→oldest and stop as soon as a
        // page reaches episodes we already cached — after the first full sync
        // that's just the newest page. The cached metadata is what makes a show
        // render instantly on reopen (and offline). MAX_PAGES bounds the first
        // (full) sync of a very large feed.
        const PAGE: i32 = 200;
        const MAX_PAGES: i32 = 200;

        let cached: HashSet<String> = repo::list_episode_guids(self.pool, podcast_id)
            .await?
            .into_iter()
            .collect();

        let mut fetched: HashSet<String> = HashSet::new();
        // The server's fresh "has it cached" flag, by episode id, for the pages
        // we actually fetched — drives loopback-vs-origin playback routing for
        // the newest episodes; older cached rows default to origin.
        let mut server_dl: HashMap<String, bool> = HashMap::new();
        let mut hit_cache = false;
        let mut reached_end = false;
        let mut offset: i32 = 0;
        for _ in 0..MAX_PAGES {
            let page = self.auth.list_episodes(podcast_id, PAGE, offset).await?;
            let full = page.len() == PAGE as usize;
            let mut page_overlaps = false;
            for e in &page {
                if cached.contains(&e.guid) {
                    page_overlaps = true;
                }
                fetched.insert(e.guid.clone());
                server_dl.insert(e.id.clone(), e.downloaded);
                // Persist metadata so the list survives an app restart / offline.
                self.cache_upsert_episode_meta(podcast_id, e).await?;
            }
            if page_overlaps {
                hit_cache = true;
                break;
            }
            if !full {
                reached_end = true;
                break;
            }
            offset += PAGE;
        }

        // Walked the whole feed with zero overlap → its identity changed; drop
        // the stale metadata (downloaded episodes are preserved). Guard on a
        // non-empty fetch so a transient empty response can't wipe the cache.
        if !cached.is_empty() && !hit_cache && reached_end && !fetched.is_empty() {
            repo::delete_stale_metadata_episodes(self.pool, podcast_id, &fetched).await?;
        }

        // Return the full cached list (everything we didn't need to re-fetch is
        // already here), newest-first, with the freshest download state we have.
        let rows = repo::list_episodes_for_podcast(self.pool, podcast_id).await?;
        let items = rows
            .into_iter()
            .map(|e| {
                let sd = server_dl.get(&e.id).copied().unwrap_or(false);
                MergedEpisode::from_cache_row(e, sd)
            })
            .collect();
        Ok(LibraryView::server(items))
    }

    /// Mirror one server episode's metadata into the cache without touching any
    /// client-owned download columns (see [`repo::upsert_episode_meta`]).
    async fn cache_upsert_episode_meta(
        &self,
        podcast_id: &str,
        e: &PodcastEpisode,
    ) -> AppResult<()> {
        let row = cache_model::PodcastEpisode {
            id: e.id.clone(),
            podcast_id: podcast_id.to_string(),
            guid: e.guid.clone(),
            title: e.title.clone(),
            description: e.description.clone(),
            enclosure_url: e.enclosure_url.clone(),
            episode_no: e.episode_no,
            season_no: e.season_no,
            duration_ms: e.duration_ms,
            // Download-only columns: NULL for a fresh metadata row, and left
            // untouched by `upsert_episode_meta` when the row is already downloaded.
            codec: None,
            bitrate_kbps: None,
            file_size: None,
            local_file_path: None,
            image_path: None,
            published_at: e.published_at.clone(),
            metadata_json: "{}".to_string(),
            downloaded_at: None,
            updated_at: now_iso(),
        };
        repo::upsert_episode_meta(self.pool, &row).await
    }

    async fn list_episodes_from_cache(
        &self,
        podcast_id: &str,
    ) -> AppResult<LibraryView<MergedEpisode>> {
        let rows = repo::list_episodes_for_podcast(self.pool, podcast_id).await?;
        Ok(LibraryView::cache(
            rows.into_iter().map(MergedEpisode::from_cache).collect(),
        ))
    }

    // ----- catalog add (Manager+ server-side) ----------------------------

    /// Add a feed to the shared catalog (Manager+). Provide a `feed_url`, or an
    /// `itunes_id` for the server to resolve. Mirrors the show into the cache.
    pub async fn subscribe_feed(
        &self,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> AppResult<MergedPodcast> {
        let p = self.auth.subscribe_feed(feed_url, itunes_id).await?;
        let subscribed = self.auth.is_subscribed(&p.id).await.unwrap_or(false);
        self.cache_upsert_podcast(&p, subscribed).await?;
        let count = repo::count_downloaded_episodes_for_podcast(self.pool, &p.id).await?;
        Ok(MergedPodcast::from_server(p, subscribed, count))
    }

    // ----- subscribe / unsubscribe (server-authoritative) ----------------

    pub async fn subscribe(&self, id: &str) -> AppResult<MergedPodcast> {
        self.auth.subscribe_podcast(id).await?;
        // Refresh the show + mirror the subscribed flag into the cache.
        self.get_podcast(id).await
    }

    pub async fn unsubscribe(&self, id: &str) -> AppResult<MergedPodcast> {
        self.auth.unsubscribe_podcast(id).await?;
        // Mirror immediately so an offline view reflects it; the row stays so
        // any downloaded episodes remain accessible.
        repo::set_podcast_subscribed(self.pool, id, false).await?;
        self.get_podcast(id).await
    }

    // ----- catalog mutations (Manager+ server-side) ----------------------

    pub async fn refresh(&self, id: &str) -> AppResult<RefreshReport> {
        self.auth.refresh_podcast(id).await
    }

    pub async fn set_auto_download(&self, id: &str, n: i32) -> AppResult<MergedPodcast> {
        let p = self.auth.set_podcast_auto_download(id, n).await?;
        let subscribed = self.auth.is_subscribed(id).await.unwrap_or(false);
        self.cache_upsert_podcast(&p, subscribed).await?;
        let count = repo::count_downloaded_episodes_for_podcast(self.pool, id).await?;
        Ok(MergedPodcast::from_server(p, subscribed, count))
    }

    // ----- internals ------------------------------------------------------

    async fn cache_upsert_podcast(&self, p: &Podcast, subscribed: bool) -> AppResult<()> {
        let row = cache_model::Podcast {
            id: p.id.clone(),
            feed_url: p.feed_url.clone(),
            title: p.title.clone(),
            author: p.author.clone(),
            description: p.description.clone(),
            image_url: p.image_url.clone(),
            language: p.language.clone(),
            categories: serde_json::to_string(&p.categories).unwrap_or_else(|_| "[]".into()),
            subscribed: if subscribed { 1 } else { 0 },
            updated_at: now_iso(),
        };
        repo::upsert_podcast(self.pool, &row).await
    }
}
