//! Podcast service: server-backed reads when online, cache-only fallback when
//! offline. Mirrors `library::service` — each result carries its offline state.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;

use super::merged::{MergedEpisode, MergedPodcast};
use crate::auth::AuthManager;
use crate::cache::model as cache_model;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::library::LibraryView;
use crate::transport::{Podcast, PodcastCandidate, RefreshReport};

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
        // The server caps a single response at 200, so page through the whole
        // feed — the show detail view wants every episode, not just the newest
        // page. MAX_PAGES is a safety valve against a misbehaving feed.
        const PAGE: i32 = 200;
        const MAX_PAGES: i32 = 200;
        let mut eps = Vec::new();
        let mut offset: i32 = 0;
        for _ in 0..MAX_PAGES {
            let page = self.auth.list_episodes(podcast_id, PAGE, offset).await?;
            let full = page.len() == PAGE as usize;
            eps.extend(page);
            if !full {
                break;
            }
            offset += PAGE;
        }
        // One cache query → id → local_file_path for downloaded episodes.
        let cached = repo::list_episodes_for_podcast(self.pool, podcast_id).await?;
        let local: HashMap<String, String> = cached
            .into_iter()
            .filter_map(|e| e.local_file_path.map(|p| (e.id, p)))
            .collect();
        let items = eps
            .into_iter()
            .map(|e| {
                let lp = local.get(&e.id).cloned();
                MergedEpisode::from_server(e, lp)
            })
            .collect();
        Ok(LibraryView::server(items))
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
