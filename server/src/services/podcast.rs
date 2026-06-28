//! Podcast orchestration.
//!
//! Ties the directory ([`PodcastDirectory`]), feed parser
//! ([`crate::services::feed`]), and repos together. A podcast is a catalog show
//! (like an artist); episodes are on-disk audio files (like tracks) downloaded
//! under `PODCAST_PATH` and served by the same byte-range streaming. New
//! episodes reuse the notification fan-out. The whole feature is gated on a
//! configured `podcast_root`.
//!
//! Permissions (server-side, defense in depth):
//! - **catalog** (subscribe a feed, delete, refresh, set auto-download): Manager+,
//!   audited — it writes to the shared library.
//! - **subscriptions** (a user follows a show for alerts): any authed user,
//!   `SECRET_KEY` rejected (no user to own it), audited.
//! - **search / reads / download**: any authed user (downloads are for offline
//!   use, like the music permission model).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tracing::warn;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    NewAuditEntry, NewPodcast, NewPodcastEpisode, PermissionLevel, Podcast, PodcastEpisode,
};
use crate::db::repo::{AuditRepo, PodcastEpisodeRepo, PodcastRepo, PodcastSubscriptionRepo};
use crate::error::{AppError, Result};
use crate::services::organizer::sanitize;
use crate::services::podcast_dir::{PodcastCandidate, PodcastDirectory};
use crate::services::{duration, feed, tag, NotificationService};

const MAX_PAGE_LIMIT: i64 = 200;
const DEFAULT_PAGE_LIMIT: i64 = 50;
/// Floor for the refresh poller cadence, regardless of config.
const MIN_REFRESH_SECS: u64 = 60;
/// Safety valve on how many `rel="next"` pages a single walk will follow, so a
/// misbehaving (cyclic / unbounded) paged feed can't loop forever.
const MAX_FEED_PAGES: u32 = 50;

/// Report of one feed refresh.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshReport {
    pub podcast_id: Uuid,
    /// Number of genuinely-new episodes ingested this refresh.
    pub new_episodes: u64,
    /// `true` when the feed returned `304 Not Modified` (cheap no-op).
    pub not_modified: bool,
}

/// Outcome of a conditional feed fetch. `parsed` is boxed because a
/// `ParsedFeed` is much larger than the `NotModified` variant.
enum FetchOutcome {
    NotModified,
    Fetched {
        parsed: Box<feed::ParsedFeed>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// Outcome of walking a feed's pages (see [`PodcastService::walk_feed`]).
struct WalkResult {
    /// Episodes inserted this walk (didn't exist in the cache before) — the
    /// genuinely-new ones to fan out.
    new_eps: Vec<PodcastEpisode>,
    /// Every episode guid seen across the fetched pages.
    fetched: HashSet<String>,
    /// A fetched page overlapped the pre-walk cache snapshot.
    hit_cache: bool,
    /// The walk reached the end of the feed (ran out of `rel="next"` pages or
    /// hit the page cap) rather than stopping early on a cache hit.
    reached_end: bool,
}

#[derive(Clone)]
pub struct PodcastService {
    podcasts: Arc<dyn PodcastRepo>,
    episodes: Arc<dyn PodcastEpisodeRepo>,
    subscriptions: Arc<dyn PodcastSubscriptionRepo>,
    audit: Arc<dyn AuditRepo>,
    directory: Arc<dyn PodcastDirectory>,
    /// New-episode fan-out. `None` disables alerts (still functional otherwise).
    notifications: Option<NotificationService>,
    http: reqwest::Client,
    /// Where episode audio + show art are stored (`PODCAST_PATH`).
    podcast_root: PathBuf,
    /// Default newest-N auto-download for a freshly-subscribed show.
    auto_download_default: i32,
    /// Refresh poller cadence (seconds).
    refresh_interval_secs: u64,
}

impl PodcastService {
    pub fn new(
        podcasts: Arc<dyn PodcastRepo>,
        episodes: Arc<dyn PodcastEpisodeRepo>,
        subscriptions: Arc<dyn PodcastSubscriptionRepo>,
        audit: Arc<dyn AuditRepo>,
        directory: Arc<dyn PodcastDirectory>,
        podcast_root: PathBuf,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("music-server/", env!("CARGO_PKG_VERSION"), " ( podcasts )"))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("reqwest client build");
        Self {
            podcasts,
            episodes,
            subscriptions,
            audit,
            directory,
            notifications: None,
            http,
            podcast_root,
            auto_download_default: 0,
            refresh_interval_secs: 1800,
        }
    }

    pub fn with_notifications(mut self, notifications: NotificationService) -> Self {
        self.notifications = Some(notifications);
        self
    }

    pub fn with_auto_download_default(mut self, n: i32) -> Self {
        self.auto_download_default = n.max(0);
        self
    }

    pub fn with_refresh_interval(mut self, secs: u64) -> Self {
        self.refresh_interval_secs = secs;
        self
    }

    pub fn podcast_root(&self) -> &Path {
        &self.podcast_root
    }

    // -----------------------------------------------------------------------
    // Discovery + catalog (Manager+ for mutations)
    // -----------------------------------------------------------------------

    /// Search the directory for shows. Any authed user.
    pub async fn search(
        &self,
        caller: &Identity,
        term: &str,
        limit: i64,
    ) -> Result<Vec<PodcastCandidate>> {
        caller.require(PermissionLevel::User)?;
        if term.trim().is_empty() {
            return Err(AppError::InvalidArgument("search term is required".into()));
        }
        self.directory.search(term, clamp_limit(Some(limit))).await
    }

    /// Subscribe the catalog to a feed (Manager+, audited `podcast.create`).
    /// Provide a `feed_url` directly, or an `itunes_id` to resolve one via the
    /// directory. Fetches + parses the feed, upserts the show, caches its art,
    /// and seeds episode metadata. Idempotent on the feed URL.
    pub async fn subscribe_feed(
        &self,
        caller: &Identity,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> Result<Podcast> {
        caller.require(PermissionLevel::Manager)?;

        // Resolve the feed URL (+ any directory ids/art the candidate carries).
        let (feed_url, cand): (String, Option<PodcastCandidate>) = match feed_url {
            Some(u) if !u.trim().is_empty() => (u.trim().to_string(), None),
            _ => {
                let id = itunes_id.ok_or_else(|| {
                    AppError::InvalidArgument("feed_url or itunes_id is required".into())
                })?;
                let c = self
                    .directory
                    .lookup(id)
                    .await?
                    .ok_or_else(|| AppError::NotFound(format!("podcast directory id {id}")))?;
                (c.feed_url.clone(), Some(c))
            }
        };

        let FetchOutcome::Fetched { parsed, etag, last_modified } =
            self.fetch_and_parse(&feed_url, None).await?
        else {
            // `None` conditional never yields NotModified.
            return Err(AppError::Internal("feed fetch returned not-modified".into()));
        };

        let categories = if !parsed.categories.is_empty() {
            parsed.categories.clone()
        } else {
            cand.as_ref().map(|c| c.categories.clone()).unwrap_or_default()
        };
        let categories_json = serde_json::to_string(&categories).unwrap_or_else(|_| "[]".into());

        let podcast = self
            .podcasts
            .upsert_by_feed_url(NewPodcast {
                feed_url: feed_url.clone(),
                title: parsed.title.clone(),
                author: parsed.author.clone(),
                description: parsed.description.clone(),
                image_url: parsed
                    .image_url
                    .clone()
                    .or_else(|| cand.as_ref().and_then(|c| c.image_url.clone())),
                link: parsed.link.clone(),
                language: parsed.language.clone(),
                categories: categories_json,
                itunes_id: itunes_id.or_else(|| cand.as_ref().and_then(|c| c.itunes_id)),
                podcastindex_id: cand.as_ref().and_then(|c| c.podcastindex_id),
                auto_download: self.auto_download_default,
            })
            .await?;

        // Seed the back-catalog in the background. Walking a paged feed to its
        // end (caching show art + every episode) can take many seconds on large
        // shows, and the client only needs the show record to navigate to it —
        // episodes stream in as the walk completes. No fan-out on first
        // subscribe (every episode is "new"; we don't blast a backlog at
        // subscribers).
        {
            let this = self.clone();
            let podcast = podcast.clone();
            tokio::spawn(async move {
                this.cache_show_art(&podcast).await;
                if let Err(e) = this.walk_feed(&podcast, *parsed, &HashSet::new(), false).await {
                    warn!(podcast = %podcast.id, error = %e, "podcast subscribe: back-catalog seed failed");
                    return;
                }
                if let Err(e) = this
                    .podcasts
                    .touch_refreshed(podcast.id, etag.as_deref(), last_modified.as_deref())
                    .await
                {
                    warn!(podcast = %podcast.id, error = %e, "podcast subscribe: touch_refreshed failed");
                }
            });
        }

        let fresh = self.podcasts.get(podcast.id).await?.unwrap_or(podcast);
        self.audit(
            caller,
            "podcast.create",
            fresh.id,
            None,
            Some(serde_json::to_value(&fresh).unwrap_or_default()),
        )
        .await?;
        Ok(fresh)
    }

    /// List catalog shows (paged). Any authed user. Returns `(items, total)`.
    pub async fn list(
        &self,
        caller: &Identity,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<(Vec<Podcast>, i64)> {
        caller.require(PermissionLevel::User)?;
        let items = self
            .podcasts
            .list(clamp_limit(limit), offset.unwrap_or(0).max(0))
            .await?;
        let total = self.podcasts.count().await?;
        Ok((items, total))
    }

    /// Get one show. Any authed user.
    pub async fn get(&self, caller: &Identity, id: Uuid) -> Result<Podcast> {
        caller.require(PermissionLevel::User)?;
        self.podcasts
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("podcast {id}")))
    }

    /// Delete a show (Manager+, audited). Removes its on-disk files; the DB
    /// cascade drops episodes + subscriptions.
    pub async fn delete(&self, caller: &Identity, id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::Manager)?;
        let podcast = self
            .podcasts
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("podcast {id}")))?;

        // Remove the show's on-disk tree (downloaded episodes + cached cover).
        let dir = self.show_dir(&podcast.title);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(podcast = %id, dir = %dir.display(), error = %e, "podcast delete: dir cleanup failed");
        }
        self.podcasts.delete(id).await?;
        self.audit(
            caller,
            "podcast.delete",
            id,
            Some(serde_json::to_value(&podcast).unwrap_or_default()),
            None,
        )
        .await?;
        Ok(())
    }

    /// Set the per-show auto-download policy (newest-N; 0 = metadata only).
    /// Manager+, audited.
    pub async fn set_auto_download(&self, caller: &Identity, id: Uuid, n: i32) -> Result<Podcast> {
        caller.require(PermissionLevel::Manager)?;
        let updated = self
            .podcasts
            .set_auto_download(id, n.max(0))
            .await?
            .ok_or_else(|| AppError::NotFound(format!("podcast {id}")))?;
        self.audit(
            caller,
            "podcast.update",
            id,
            None,
            Some(serde_json::json!({ "auto_download": n.max(0) })),
        )
        .await?;
        Ok(updated)
    }

    // -----------------------------------------------------------------------
    // Episodes
    // -----------------------------------------------------------------------

    /// List a show's episodes (newest first, paged). Any authed user.
    pub async fn list_episodes(
        &self,
        caller: &Identity,
        podcast_id: Uuid,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<PodcastEpisode>> {
        caller.require(PermissionLevel::User)?;
        self.episodes
            .list_for_podcast(podcast_id, clamp_limit(limit), offset.unwrap_or(0).max(0))
            .await
    }

    /// Get one episode. Any authed user.
    pub async fn get_episode(&self, caller: &Identity, id: Uuid) -> Result<PodcastEpisode> {
        caller.require(PermissionLevel::User)?;
        self.episodes
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("episode {id}")))
    }

    /// Download an episode's audio to disk (any authed user). Idempotent — an
    /// already-downloaded episode with its file present is a no-op.
    pub async fn download_episode(
        &self,
        caller: &Identity,
        episode_id: Uuid,
    ) -> Result<PodcastEpisode> {
        caller.require(PermissionLevel::User)?;
        self.download_episode_inner(episode_id).await
    }

    /// The download body, without the permission check — also used by the
    /// auto-download path (system-initiated).
    async fn download_episode_inner(&self, episode_id: Uuid) -> Result<PodcastEpisode> {
        let ep = self
            .episodes
            .get(episode_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("episode {episode_id}")))?;

        // Idempotent: a present file → no-op.
        if let Some(fp) = &ep.file_path
            && tokio::fs::metadata(fp).await.is_ok()
        {
            return Ok(ep);
        }

        let podcast = self
            .podcasts
            .get(ep.podcast_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("podcast {}", ep.podcast_id)))?;

        let dest = self.episode_dest(&podcast.title, &ep);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let size = self.stream_to_file(&ep.enclosure_url, &dest).await?;

        // Probe codec/bitrate/duration off the downloaded file (blocking IO).
        let probe_path = dest.clone();
        let (codec, bitrate, dur) = tokio::task::spawn_blocking(move || probe_audio(&probe_path))
            .await
            .map_err(|e| AppError::Internal(format!("probe join: {e}")))?;

        let updated = self
            .episodes
            .set_file(
                episode_id,
                &dest.to_string_lossy(),
                Some(size as i64),
                codec.as_deref(),
                bitrate,
                dur,
            )
            .await?
            .ok_or_else(|| AppError::NotFound(format!("episode {episode_id}")))?;
        Ok(updated)
    }

    // -----------------------------------------------------------------------
    // Refresh (new-episode detection) + poller
    // -----------------------------------------------------------------------

    /// Refresh one feed on demand (Manager+) — the "check for new episodes"
    /// action. Fans out notifications + triggers auto-download for new episodes.
    pub async fn refresh(&self, caller: &Identity, podcast_id: Uuid) -> Result<RefreshReport> {
        caller.require(PermissionLevel::Manager)?;
        self.refresh_inner(podcast_id).await
    }

    /// Refresh without a permission check — the poller path (system-initiated).
    async fn refresh_inner(&self, podcast_id: Uuid) -> Result<RefreshReport> {
        let podcast = self
            .podcasts
            .get(podcast_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("podcast {podcast_id}")))?;

        let outcome = self
            .fetch_and_parse(
                &podcast.feed_url,
                Some((podcast.last_etag.clone(), podcast.last_modified.clone())),
            )
            .await?;

        match outcome {
            FetchOutcome::NotModified => {
                self.podcasts
                    .touch_refreshed(
                        podcast_id,
                        podcast.last_etag.as_deref(),
                        podcast.last_modified.as_deref(),
                    )
                    .await?;
                Ok(RefreshReport {
                    podcast_id,
                    new_episodes: 0,
                    not_modified: true,
                })
            }
            FetchOutcome::Fetched { parsed, etag, last_modified } => {
                // Snapshot the cache, then walk the feed newest→oldest, stopping
                // as soon as a page reaches episodes we already have — everything
                // older is, by construction, already cached.
                let cached: HashSet<String> =
                    self.episodes.all_guids(podcast_id).await?.into_iter().collect();
                let walk = self.walk_feed(&podcast, *parsed, &cached, true).await?;

                // Zero overlap after walking the whole feed → the feed's identity
                // changed (e.g. it moved hosts and reissued guids). Replace the
                // stale cached metadata with what we just fetched, and don't fan
                // out the whole new feed as "new episodes". Guard on a non-empty
                // fetch so a transient empty/broken feed can never wipe the cache.
                let full_replace = !cached.is_empty()
                    && !walk.hit_cache
                    && walk.reached_end
                    && !walk.fetched.is_empty();

                if full_replace {
                    let keep: Vec<String> = walk.fetched.iter().cloned().collect();
                    let removed = self.episodes.delete_stale_metadata(podcast_id, &keep).await?;
                    warn!(
                        podcast = %podcast_id, removed,
                        "podcast refresh: feed shares nothing with cache; replaced cached episodes"
                    );
                } else {
                    self.fan_out_new(&podcast, &walk.new_eps).await;
                }

                self.podcasts
                    .touch_refreshed(podcast_id, etag.as_deref(), last_modified.as_deref())
                    .await?;
                Ok(RefreshReport {
                    podcast_id,
                    new_episodes: walk.new_eps.len() as u64,
                    not_modified: false,
                })
            }
        }
    }

    /// Spawn the background refresh poller (mirrors the image-optimize pass /
    /// upload stall sweeper in `main.rs`). Each tick refreshes every show; one
    /// bad feed never stalls the rest.
    pub fn spawn_refresh_poller(&self) {
        let this = self.clone();
        let period = Duration::from_secs(self.refresh_interval_secs.max(MIN_REFRESH_SECS));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                let podcasts = match this.podcasts.all_for_refresh().await {
                    Ok(p) => p,
                    Err(e) => {
                        warn!(error = %e, "podcast poller: list failed");
                        continue;
                    }
                };
                for p in podcasts {
                    if let Err(e) = this.refresh_inner(p.id).await {
                        warn!(podcast = %p.id, error = %e, "podcast poller: refresh failed");
                    }
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // Subscriptions (any user; SECRET_KEY rejected) — for notifications
    // -----------------------------------------------------------------------

    pub async fn subscribe(&self, caller: &Identity, podcast_id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        if self.podcasts.get(podcast_id).await?.is_none() {
            return Err(AppError::NotFound(format!("podcast {podcast_id}")));
        }
        self.subscriptions.subscribe(user_id, podcast_id).await?;
        self.audit(
            caller,
            "podcast.subscribe",
            podcast_id,
            None,
            Some(serde_json::json!({ "user_id": user_id, "podcast_id": podcast_id })),
        )
        .await?;
        Ok(())
    }

    pub async fn unsubscribe(&self, caller: &Identity, podcast_id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.subscriptions.unsubscribe(user_id, podcast_id).await?;
        self.audit(
            caller,
            "podcast.unsubscribe",
            podcast_id,
            Some(serde_json::json!({ "user_id": user_id, "podcast_id": podcast_id })),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn is_subscribed(&self, caller: &Identity, podcast_id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        Ok(self
            .subscriptions
            .subscriptions(user_id)
            .await?
            .contains(&podcast_id))
    }

    pub async fn list_subscriptions(&self, caller: &Identity) -> Result<Vec<Podcast>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let ids = self.subscriptions.subscriptions(user_id).await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(p) = self.podcasts.get(id).await? {
                out.push(p);
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    /// Walk a feed newest→oldest, upserting every episode. `cached` is the set
    /// of guids already stored before this walk began. With `stop_at_cache`, the
    /// walk halts at the first page that overlaps `cached` (the incremental
    /// refresh — pages older than the overlap are already cached); otherwise it
    /// follows every `rel="next"` page to the end (the seed-all path used on
    /// first subscribe). Returns the inserted episodes plus the bookkeeping the
    /// caller needs to decide on a full-cache replace.
    async fn walk_feed(
        &self,
        podcast: &Podcast,
        first: feed::ParsedFeed,
        cached: &HashSet<String>,
        stop_at_cache: bool,
    ) -> Result<WalkResult> {
        let mut new_eps: Vec<PodcastEpisode> = Vec::new();
        let mut fetched: HashSet<String> = HashSet::new();
        let mut hit_cache = false;
        let mut reached_end = false;
        let mut page = first;
        let mut pages: u32 = 0;
        loop {
            pages += 1;
            let page_overlaps = page.episodes.iter().any(|e| cached.contains(&e.guid));
            if page_overlaps {
                hit_cache = true;
            }
            for pe in &page.episodes {
                fetched.insert(pe.guid.clone());
                let (ep, inserted) = self
                    .episodes
                    .upsert_by_guid(NewPodcastEpisode {
                        podcast_id: podcast.id,
                        guid: pe.guid.clone(),
                        title: pe.title.clone(),
                        description: pe.description.clone(),
                        enclosure_url: pe.enclosure_url.clone(),
                        enclosure_type: pe.enclosure_type.clone(),
                        episode_no: pe.episode_no,
                        season_no: pe.season_no,
                        duration_ms: pe.duration_ms,
                        image_path: None,
                        published_at: pe.published_at,
                    })
                    .await?;
                if inserted {
                    new_eps.push(ep);
                }
            }
            // Stop once we've reached cached territory (incremental refresh).
            if stop_at_cache && page_overlaps {
                break;
            }
            // Otherwise follow the feed to its next (older) page, if any.
            match page.next_page_url.clone() {
                Some(next) if pages < MAX_FEED_PAGES => {
                    match self.fetch_and_parse(&next, None).await? {
                        FetchOutcome::Fetched { parsed, .. } => page = *parsed,
                        // A `None` conditional never yields NotModified; treat it
                        // defensively as the end of the walk.
                        FetchOutcome::NotModified => {
                            reached_end = true;
                            break;
                        }
                    }
                }
                _ => {
                    reached_end = true;
                    break;
                }
            }
        }
        Ok(WalkResult {
            new_eps,
            fetched,
            hit_cache,
            reached_end,
        })
    }

    /// Best-effort alert + auto-download for genuinely-new episodes. Never fails
    /// the refresh (same contract as `create_album`'s fan-out).
    async fn fan_out_new(&self, podcast: &Podcast, new_eps: &[PodcastEpisode]) {
        if new_eps.is_empty() {
            return;
        }
        // Fan out alerts to subscribers.
        if let Some(notifications) = &self.notifications {
            let subs = self
                .subscriptions
                .subscribers_of(podcast.id)
                .await
                .unwrap_or_default();
            if !subs.is_empty() {
                for ep in new_eps {
                    if let Err(e) = notifications.notify_new_episode(&subs, podcast, ep).await {
                        warn!(podcast = %podcast.id, error = %e, "new-episode fan-out failed");
                    }
                }
            }
        }
        // Auto-download newest-N (background — never blocks the refresh).
        if podcast.auto_download > 0 {
            let this = self.clone();
            let pid = podcast.id;
            let n = podcast.auto_download as i64;
            tokio::spawn(async move {
                let newest = this
                    .episodes
                    .newest_undownloaded(pid, n)
                    .await
                    .unwrap_or_default();
                for ep in newest {
                    if let Err(e) = this.download_episode_inner(ep.id).await {
                        warn!(episode = %ep.id, error = %e, "auto-download failed");
                    }
                }
            });
        }
    }

    /// Conditional GET + parse a feed. `conditional` carries the stored
    /// `(etag, last_modified)` for a cheap `304` when nothing changed.
    async fn fetch_and_parse(
        &self,
        feed_url: &str,
        conditional: Option<(Option<String>, Option<String>)>,
    ) -> Result<FetchOutcome> {
        let mut req = self.http.get(feed_url);
        if let Some((etag, last_modified)) = conditional {
            // Pass the stored validators straight through; `RequestBuilder::header`
            // accepts a `String` (TryFrom<String> for HeaderValue) and defers any
            // invalid-value error to `send()`.
            if let Some(e) = etag.filter(|s| !s.is_empty()) {
                req = req.header(IF_NONE_MATCH, e);
            }
            if let Some(m) = last_modified.filter(|s| !s.is_empty()) {
                req = req.header(IF_MODIFIED_SINCE, m);
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("feed fetch: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified);
        }
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "feed fetch status {}",
                resp.status()
            )));
        }
        let etag = header_string(resp.headers().get(ETAG));
        let last_modified = header_string(resp.headers().get(LAST_MODIFIED));
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("feed body: {e}")))?;
        let parsed = feed::parse_feed(&bytes)?;
        Ok(FetchOutcome::Fetched {
            parsed: Box::new(parsed),
            etag,
            last_modified,
        })
    }

    /// Best-effort: fetch the show's remote artwork and cache it under the show
    /// dir, pointing `image_path` at it. A failure leaves `image_url` in place.
    async fn cache_show_art(&self, podcast: &Podcast) {
        let Some(url) = podcast.image_url.as_deref() else {
            return;
        };
        let dir = self.show_dir(&podcast.title);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!(podcast = %podcast.id, error = %e, "show art: mkdir failed");
            return;
        }
        let resp = match self.http.get(url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(podcast = %podcast.id, status = %r.status(), "show art: non-2xx");
                return;
            }
            Err(e) => {
                warn!(podcast = %podcast.id, error = %e, "show art: fetch failed");
                return;
            }
        };
        let ext = ext_from_content_type(header_string(resp.headers().get(CONTENT_TYPE)).as_deref());
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                warn!(podcast = %podcast.id, error = %e, "show art: body failed");
                return;
            }
        };
        let dest = dir.join(format!("cover.{ext}"));
        if let Err(e) = tokio::fs::write(&dest, &bytes).await {
            warn!(podcast = %podcast.id, error = %e, "show art: write failed");
            return;
        }
        if let Err(e) = self
            .podcasts
            .set_image(podcast.id, Some(&dest.to_string_lossy()))
            .await
        {
            warn!(podcast = %podcast.id, error = %e, "show art: set_image failed");
        }
    }

    /// Stream `url` to `dest` via a `.part` temp + atomic rename. Returns bytes.
    async fn stream_to_file(&self, url: &str, dest: &Path) -> Result<u64> {
        use futures_util::StreamExt;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("episode download: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "episode download status {}",
                resp.status()
            )));
        }
        let part = dest.with_extension("part");
        let mut file = tokio::fs::File::create(&part).await?;
        let mut stream = resp.bytes_stream();
        let mut total: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AppError::Internal(format!("episode chunk: {e}")))?;
            file.write_all(&chunk).await?;
            total += chunk.len() as u64;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&part, dest).await?;
        Ok(total)
    }

    /// `<podcast_root>/<Show>` (sanitised, traversal-safe).
    fn show_dir(&self, title: &str) -> PathBuf {
        self.podcast_root.join(sanitize(title))
    }

    /// `<podcast_root>/<Show>/<NNN - Episode>.<ext>` (sanitised).
    fn episode_dest(&self, podcast_title: &str, ep: &PodcastEpisode) -> PathBuf {
        let stem = match ep.episode_no {
            Some(n) => format!("{:03} - {}", n, sanitize(&ep.title)),
            None => sanitize(&ep.title),
        };
        let ext = ext_from_enclosure(&ep.enclosure_url, ep.enclosure_type.as_deref());
        self.show_dir(podcast_title).join(format!("{stem}.{ext}"))
    }

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity has no user to subscribe podcasts; log in as a user".into(),
            )
        })
    }

    async fn audit(
        &self,
        caller: &Identity,
        action: &str,
        podcast_id: Uuid,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> Result<()> {
        let to_json = |v: Option<serde_json::Value>| -> Result<Option<String>> {
            match v {
                Some(v) => Ok(Some(
                    serde_json::to_string(&v)
                        .map_err(|e| AppError::Internal(format!("audit json: {e}")))?,
                )),
                None => Ok(None),
            }
        };
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: action.to_string(),
                entity_type: "podcast".to_string(),
                entity_id: Some(podcast_id),
                before_json: to_json(before)?,
                after_json: to_json(after)?,
            })
            .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

fn header_string(v: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    v.and_then(|h| h.to_str().ok()).map(|s| s.to_string())
}

/// Probe a downloaded audio file for `(codec, bitrate_kbps, duration_ms)`.
/// Reuses the music library's tag + duration machinery (the MP3 frame-walker
/// matters — podcasts are mostly VBR MP3). Blocking; call via `spawn_blocking`.
fn probe_audio(path: &Path) -> (Option<String>, Option<i32>, Option<i64>) {
    let measured = duration::measure_duration(path).map(|d| d.as_millis() as i64);
    match tag::read_tags(path) {
        Ok(t) => {
            let dur = measured.or(if t.duration_ms > 0 {
                Some(t.duration_ms)
            } else {
                None
            });
            (Some(t.codec), t.bitrate_kbps, dur)
        }
        Err(_) => (None, None, measured),
    }
}

/// Audio file extension from the enclosure URL path, falling back to the
/// content-type, then `mp3`.
fn ext_from_enclosure(url: &str, content_type: Option<&str>) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    for ext in ["mp3", "m4a", "aac", "ogg", "opus", "flac", "wav", "mp4"] {
        if path.ends_with(&format!(".{ext}")) {
            return ext.to_string();
        }
    }
    match content_type.map(|c| c.split(';').next().unwrap_or(c).trim().to_ascii_lowercase()) {
        Some(ct) => match ct.as_str() {
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
            "audio/aac" => "aac",
            "audio/ogg" | "application/ogg" => "ogg",
            "audio/opus" => "opus",
            "audio/flac" | "audio/x-flac" => "flac",
            "audio/wav" | "audio/x-wav" => "wav",
            _ => "mp3",
        }
        .to_string(),
        None => "mp3".to_string(),
    }
}

/// Image extension from a content-type (`jpg` fallback).
fn ext_from_content_type(content_type: Option<&str>) -> &'static str {
    match content_type.map(|c| c.split(';').next().unwrap_or(c).trim().to_ascii_lowercase()) {
        Some(ct) => match ct.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        },
        None => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AuditEntry, NewAuditEntry};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // ---- Fake repos (only what the service tests exercise) ----

    #[derive(Default)]
    struct FakePodcasts {
        rows: Mutex<Vec<Podcast>>,
    }
    impl FakePodcasts {
        fn insert(&self, title: &str) -> Podcast {
            let p = Podcast {
                id: Uuid::new_v4(),
                feed_url: format!("https://feeds.example.com/{title}"),
                title: title.to_string(),
                author: None,
                description: None,
                image_path: None,
                image_url: None,
                link: None,
                language: None,
                categories: "[]".into(),
                itunes_id: None,
                podcastindex_id: None,
                auto_download: 0,
                storage_bytes: 0,
                last_refreshed_at: None,
                last_etag: None,
                last_modified: None,
                created_at: now(),
                updated_at: now(),
            };
            self.rows.lock().unwrap().push(p.clone());
            p
        }
    }
    #[async_trait]
    impl PodcastRepo for FakePodcasts {
        async fn upsert_by_feed_url(&self, _: NewPodcast) -> Result<Podcast> {
            unimplemented!("network path; not unit-tested")
        }
        async fn get(&self, id: Uuid) -> Result<Option<Podcast>> {
            Ok(self.rows.lock().unwrap().iter().find(|p| p.id == id).cloned())
        }
        async fn get_by_feed_url(&self, url: &str) -> Result<Option<Podcast>> {
            Ok(self.rows.lock().unwrap().iter().find(|p| p.feed_url == url).cloned())
        }
        async fn list(&self, _: i64, _: i64) -> Result<Vec<Podcast>> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn count(&self) -> Result<i64> {
            Ok(self.rows.lock().unwrap().len() as i64)
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Podcast>> {
            Ok(vec![])
        }
        async fn set_image(&self, _: Uuid, _: Option<&str>) -> Result<Option<Podcast>> {
            Ok(None)
        }
        async fn set_auto_download(&self, id: Uuid, n: i32) -> Result<Option<Podcast>> {
            let mut g = self.rows.lock().unwrap();
            if let Some(p) = g.iter_mut().find(|p| p.id == id) {
                p.auto_download = n;
                return Ok(Some(p.clone()));
            }
            Ok(None)
        }
        async fn touch_refreshed(&self, _: Uuid, _: Option<&str>, _: Option<&str>) -> Result<()> {
            Ok(())
        }
        async fn all_for_refresh(&self) -> Result<Vec<Podcast>> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn delete(&self, id: Uuid) -> Result<()> {
            self.rows.lock().unwrap().retain(|p| p.id != id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeEpisodes {
        rows: Mutex<Vec<PodcastEpisode>>,
    }
    #[async_trait]
    impl PodcastEpisodeRepo for FakeEpisodes {
        async fn upsert_by_guid(&self, new: NewPodcastEpisode) -> Result<(PodcastEpisode, bool)> {
            let mut g = self.rows.lock().unwrap();
            if let Some(e) = g
                .iter()
                .find(|e| e.podcast_id == new.podcast_id && e.guid == new.guid)
                .cloned()
            {
                return Ok((e, false)); // already present → not new
            }
            let ep = PodcastEpisode {
                id: Uuid::new_v4(),
                podcast_id: new.podcast_id,
                guid: new.guid,
                title: new.title,
                description: new.description,
                enclosure_url: new.enclosure_url,
                enclosure_type: new.enclosure_type,
                episode_no: new.episode_no,
                season_no: new.season_no,
                duration_ms: new.duration_ms,
                codec: None,
                bitrate_kbps: None,
                file_path: None,
                file_size: None,
                image_path: new.image_path,
                published_at: new.published_at,
                metadata_json: "{}".into(),
                created_at: now(),
                updated_at: now(),
            };
            g.push(ep.clone());
            Ok((ep, true))
        }
        async fn get(&self, id: Uuid) -> Result<Option<PodcastEpisode>> {
            Ok(self.rows.lock().unwrap().iter().find(|e| e.id == id).cloned())
        }
        async fn list_for_podcast(&self, pid: Uuid, _: i64, _: i64) -> Result<Vec<PodcastEpisode>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.podcast_id == pid)
                .cloned()
                .collect())
        }
        async fn newest_undownloaded(&self, _: Uuid, _: i64) -> Result<Vec<PodcastEpisode>> {
            Ok(vec![])
        }
        async fn all_guids(&self, pid: Uuid) -> Result<Vec<String>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.podcast_id == pid)
                .map(|e| e.guid.clone())
                .collect())
        }
        async fn delete_stale_metadata(&self, pid: Uuid, keep: &[String]) -> Result<u64> {
            let mut g = self.rows.lock().unwrap();
            let before = g.len();
            g.retain(|e| {
                !(e.podcast_id == pid && e.file_path.is_none() && !keep.contains(&e.guid))
            });
            Ok((before - g.len()) as u64)
        }
        async fn set_file(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i64>,
            _: Option<&str>,
            _: Option<i32>,
            _: Option<i64>,
        ) -> Result<Option<PodcastEpisode>> {
            Ok(None)
        }
        async fn clear_file(&self, _: Uuid) -> Result<Option<PodcastEpisode>> {
            Ok(None)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSubs {
        rows: Mutex<Vec<(Uuid, Uuid)>>, // (user, podcast)
    }
    #[async_trait]
    impl PodcastSubscriptionRepo for FakeSubs {
        async fn subscribe(&self, user_id: Uuid, podcast_id: Uuid) -> Result<()> {
            let mut g = self.rows.lock().unwrap();
            if !g.iter().any(|(u, p)| *u == user_id && *p == podcast_id) {
                g.push((user_id, podcast_id));
            }
            Ok(())
        }
        async fn unsubscribe(&self, user_id: Uuid, podcast_id: Uuid) -> Result<()> {
            self.rows
                .lock()
                .unwrap()
                .retain(|(u, p)| !(*u == user_id && *p == podcast_id));
            Ok(())
        }
        async fn subscribers_of(&self, podcast_id: Uuid) -> Result<Vec<Uuid>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, p)| *p == podcast_id)
                .map(|(u, _)| *u)
                .collect())
        }
        async fn subscriptions(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(u, _)| *u == user_id)
                .map(|(_, p)| *p)
                .collect())
        }
    }

    #[derive(Default)]
    struct FakeAudit {
        actions: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl AuditRepo for FakeAudit {
        async fn record(&self, e: NewAuditEntry) -> Result<AuditEntry> {
            self.actions.lock().unwrap().push(e.action.clone());
            Ok(AuditEntry {
                id: Uuid::new_v4(),
                actor_id: e.actor_id,
                action: e.action,
                entity_type: e.entity_type,
                entity_id: e.entity_id,
                before_json: e.before_json,
                after_json: e.after_json,
                created_at: now(),
            })
        }
        async fn list_for_entity(&self, _: &str, _: Uuid) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }

    struct FakeDirectory;
    #[async_trait]
    impl PodcastDirectory for FakeDirectory {
        async fn search(&self, term: &str, _: i64) -> Result<Vec<PodcastCandidate>> {
            Ok(vec![PodcastCandidate {
                feed_url: format!("https://feeds.example.com/{term}"),
                title: term.to_string(),
                ..Default::default()
            }])
        }
        async fn lookup(&self, _: i64) -> Result<Option<PodcastCandidate>> {
            Ok(None)
        }
    }

    fn make_service() -> (PodcastService, Arc<FakePodcasts>, Arc<FakeEpisodes>, Arc<FakeAudit>) {
        let podcasts = Arc::new(FakePodcasts::default());
        let episodes = Arc::new(FakeEpisodes::default());
        let subs = Arc::new(FakeSubs::default());
        let audit = Arc::new(FakeAudit::default());
        let svc = PodcastService::new(
            podcasts.clone(),
            episodes.clone(),
            subs,
            audit.clone(),
            Arc::new(FakeDirectory),
            PathBuf::from("/tmp/podcasts-test"),
        );
        (svc, podcasts, episodes, audit)
    }

    fn user() -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level: PermissionLevel::User,
        }
    }

    fn parsed_with(guids: &[&str]) -> feed::ParsedFeed {
        feed::ParsedFeed {
            title: "Show".into(),
            episodes: guids
                .iter()
                .map(|g| feed::ParsedEpisode {
                    guid: (*g).to_string(),
                    title: format!("Episode {g}"),
                    enclosure_url: format!("https://cdn.example.com/{g}.mp3"),
                    enclosure_type: Some("audio/mpeg".into()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn secret_key_cannot_subscribe() {
        let (svc, podcasts, ..) = make_service();
        let p = podcasts.insert("Daily");
        let err = svc
            .subscribe(&Identity::SecretKey, p.id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn subscribe_unknown_podcast_is_404() {
        let (svc, ..) = make_service();
        let err = svc.subscribe(&user(), Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn subscribe_roundtrip_and_audit() {
        let (svc, podcasts, _e, audit) = make_service();
        let me = user();
        let p = podcasts.insert("Daily");

        assert!(!svc.is_subscribed(&me, p.id).await.unwrap());
        svc.subscribe(&me, p.id).await.unwrap();
        // Idempotent.
        svc.subscribe(&me, p.id).await.unwrap();
        assert!(svc.is_subscribed(&me, p.id).await.unwrap());
        assert_eq!(svc.list_subscriptions(&me).await.unwrap().len(), 1);

        svc.unsubscribe(&me, p.id).await.unwrap();
        assert!(!svc.is_subscribed(&me, p.id).await.unwrap());
        assert!(svc.list_subscriptions(&me).await.unwrap().is_empty());

        let actions = audit.actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec!["podcast.subscribe", "podcast.subscribe", "podcast.unsubscribe"]
        );
    }

    #[tokio::test]
    async fn search_requires_a_term_and_delegates() {
        let (svc, ..) = make_service();
        let me = user();
        assert!(matches!(
            svc.search(&me, "  ", 10).await.unwrap_err(),
            AppError::InvalidArgument(_)
        ));
        let hits = svc.search(&me, "rust", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "rust");
    }

    /// Snapshot the fake's cached guids — what the incremental walk compares
    /// each page against.
    fn snapshot(eps: &Arc<FakeEpisodes>) -> HashSet<String> {
        eps.rows.lock().unwrap().iter().map(|e| e.guid.clone()).collect()
    }

    #[tokio::test]
    async fn walk_detects_new_episodes_idempotently() {
        let (svc, podcasts, episodes, _a) = make_service();
        let p = podcasts.insert("Show");

        // First walk: both episodes are new.
        let cached = snapshot(&episodes);
        let r = svc
            .walk_feed(&p, parsed_with(&["a", "b"]), &cached, true)
            .await
            .unwrap();
        assert_eq!(r.new_eps.len(), 2);
        assert_eq!(episodes.rows.lock().unwrap().len(), 2);

        // Re-walk the same feed → 0 new (upsert-by-guid is idempotent); the walk
        // stops on the cache hit.
        let cached = snapshot(&episodes);
        let r = svc
            .walk_feed(&p, parsed_with(&["a", "b"]), &cached, true)
            .await
            .unwrap();
        assert_eq!(r.new_eps.len(), 0);
        assert!(r.hit_cache);

        // The newest episode prepended → exactly 1 new; the rest overlap the
        // cache so the walk halts there (incremental).
        let cached = snapshot(&episodes);
        let r = svc
            .walk_feed(&p, parsed_with(&["c", "a", "b"]), &cached, true)
            .await
            .unwrap();
        assert_eq!(r.new_eps.len(), 1);
        assert!(r.hit_cache);
        assert_eq!(episodes.rows.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn walk_with_no_overlap_signals_full_replace() {
        let (svc, podcasts, episodes, _a) = make_service();
        let p = podcasts.insert("Show");

        // Seed two episodes (seed-all path — no early stop).
        svc.walk_feed(&p, parsed_with(&["a", "b"]), &HashSet::new(), false)
            .await
            .unwrap();

        // A feed that shares NOTHING with the cache: the walk runs to the end
        // without a cache hit — exactly the signal `refresh` uses to replace.
        let cached = snapshot(&episodes);
        let r = svc
            .walk_feed(&p, parsed_with(&["x", "y"]), &cached, true)
            .await
            .unwrap();
        assert!(!r.hit_cache);
        assert!(r.reached_end);
        assert_eq!(r.fetched.len(), 2);

        // Replacing keeps only the freshly-fetched guids; the stale (not yet
        // downloaded) ones are dropped.
        let keep: Vec<String> = r.fetched.iter().cloned().collect();
        let removed = episodes.delete_stale_metadata(p.id, &keep).await.unwrap();
        assert_eq!(removed, 2); // a, b
        assert_eq!(snapshot(&episodes), ["x", "y"].iter().map(|s| s.to_string()).collect());
    }

    #[tokio::test]
    async fn delete_stale_metadata_preserves_downloaded() {
        let (_svc, podcasts, episodes, _a) = make_service();
        let p = podcasts.insert("Show");

        // One downloaded episode (has a file_path) + one metadata-only.
        episodes
            .upsert_by_guid(NewPodcastEpisode {
                podcast_id: p.id,
                guid: "kept".into(),
                title: "Downloaded".into(),
                description: None,
                enclosure_url: "https://cdn/kept.mp3".into(),
                enclosure_type: None,
                episode_no: None,
                season_no: None,
                duration_ms: None,
                image_path: None,
                published_at: None,
            })
            .await
            .unwrap();
        episodes
            .rows
            .lock()
            .unwrap()
            .iter_mut()
            .for_each(|e| {
                if e.guid == "kept" {
                    e.file_path = Some("/tmp/kept.mp3".into());
                }
            });
        episodes
            .upsert_by_guid(NewPodcastEpisode {
                podcast_id: p.id,
                guid: "stale".into(),
                title: "Metadata only".into(),
                description: None,
                enclosure_url: "https://cdn/stale.mp3".into(),
                enclosure_type: None,
                episode_no: None,
                season_no: None,
                duration_ms: None,
                image_path: None,
                published_at: None,
            })
            .await
            .unwrap();

        // Replace against a brand-new feed that keeps neither guid: the
        // downloaded one survives (its audio is on disk), the metadata one goes.
        let removed = episodes.delete_stale_metadata(p.id, &["fresh".to_string()]).await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(snapshot(&episodes), ["kept"].iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn enclosure_ext_from_url_then_content_type() {
        assert_eq!(ext_from_enclosure("https://x/ep.mp3", None), "mp3");
        assert_eq!(ext_from_enclosure("https://x/ep.m4a?token=1", None), "m4a");
        assert_eq!(ext_from_enclosure("https://x/audio", Some("audio/mpeg")), "mp3");
        assert_eq!(
            ext_from_enclosure("https://x/stream", Some("audio/mp4; rate=44100")),
            "m4a"
        );
        // Unknown → mp3 default.
        assert_eq!(ext_from_enclosure("https://x/blob", Some("application/x")), "mp3");
    }

    #[test]
    fn image_ext_mapping() {
        assert_eq!(ext_from_content_type(Some("image/png")), "png");
        assert_eq!(ext_from_content_type(Some("image/jpeg")), "jpg");
        assert_eq!(ext_from_content_type(Some("image/webp; q=1")), "webp");
        assert_eq!(ext_from_content_type(None), "jpg");
    }

    #[test]
    fn clamp_limit_bounds() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10_000)), MAX_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(25)), 25);
    }
}
