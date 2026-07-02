//! `DiscographyService` — resolve, sync, diff, and suppression orchestration
//! (DISCOGRAPHY_SYNC.md §6). Manager-gated end to end (defense in depth over the
//! transport layer, per the services convention).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    Album, DiscographyIgnore, DiscographyReport, NewDiscographyIgnore, NewStoredReport,
    PermissionLevel, TrackFingerprint,
};
use crate::db::repo::{AliasRepo, AlbumRepo, ArtistRepo, DiscographyRepo, TrackRepo};
use crate::error::{AppError, Result};
use crate::services::NotificationService;

use super::diff::{apply_ignores, ProviderSnapshot, SnapMissingTrack, SnapReleaseGroup};
use super::r#match::{matches_any, normalize_title, similarity};
use super::provider::{ArtistCandidate, DiscographyProvider};

/// Hook for alerting an artist's followers when a sync detects a genuinely-new
/// missing release (Phase D). A trait (implemented by [`NotificationService`])
/// so the service stays unit-testable against a fake.
#[async_trait]
pub trait NewReleaseNotifier: Send + Sync {
    /// Fan out a "new release from this artist" alert to followers. Returns the
    /// number of notifications created.
    async fn notify(&self, artist_id: Uuid, title: &str) -> Result<u64>;
}

#[async_trait]
impl NewReleaseNotifier for NotificationService {
    async fn notify(&self, artist_id: Uuid, title: &str) -> Result<u64> {
        self.notify_provider_release(artist_id, title).await
    }
}

/// Audio-anchored artist resolution (Phase E). Given the Chromaprint
/// fingerprints of some owned tracks, resolve the artist to a provider
/// (MusicBrainz, via AcoustID) id — sharpening resolution beyond name search.
/// A trait so the service stays testable; the AcoustID impl lives behind the
/// `chromaprint` build feature.
#[async_trait]
pub trait AudioResolver: Send + Sync {
    /// The provider artist id the fingerprints point to, or `None` when the
    /// audio can't confidently resolve (→ fall back to name search).
    async fn resolve_artist(&self, fingerprints: &[TrackFingerprint]) -> Result<Option<String>>;
}

// `dominant_artist` is consumed by the AcoustID resolver (chromaprint feature)
// and the unit tests; it's dead in a default, non-test lib build.
#[cfg_attr(not(feature = "chromaprint"), allow(dead_code))]

/// Pick the dominant provider artist id across per-track resolutions (Phase E).
/// Each input is `(artist_ids_for_that_track, best_match_score)`. Accepts a
/// winner only when **two independent tracks agree**, or a single track resolves
/// unambiguously with a high score — so one featured-artist credit can't
/// mis-anchor the whole artist. Pure + unit-tested.
pub(super) fn dominant_artist(tracks: &[(Vec<String>, f32)]) -> Option<String> {
    use std::collections::{HashMap, HashSet};
    let returned: Vec<&(Vec<String>, f32)> = tracks.iter().filter(|(a, _)| !a.is_empty()).collect();
    if returned.is_empty() {
        return None;
    }
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for (artists, _) in &returned {
        let mut seen = HashSet::new();
        for a in artists.iter() {
            if seen.insert(a.as_str()) {
                *counts.entry(a.as_str()).or_default() += 1;
            }
        }
    }
    let (winner, count) = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(k, c)| (k.to_string(), *c))?;
    if count >= 2 {
        return Some(winner);
    }
    // Single-track high-confidence fallback: one track, one artist, strong score.
    if returned.len() == 1 {
        let (artists, score) = returned[0];
        if artists.len() == 1 && *score >= 0.9 {
            return Some(winner);
        }
    }
    None
}

/// How far the top candidate's score must beat the runner-up to auto-accept
/// (points) — guards against confidently picking one of two same-named artists.
const MATCH_MARGIN: u8 = 5;

/// Skip re-syncing an artist synced within this window during a background pass.
const FRESHNESS_DAYS: i64 = 7;

/// A newly-detected missing release notifies followers only if its first-release
/// year is within this many years of now — so a re-sync that surfaces a
/// freshly-*cataloged* old album doesn't masquerade as a new release.
const NEW_RELEASE_RECENT_YEARS: i32 = 1;

/// Cap on how many new-release notifications one sync fans out (a burst guard —
/// the "new since last snapshot + recent" filter already makes bursts rare).
const NEW_RELEASE_NOTIFY_CAP: usize = 20;

/// How many of an artist's fingerprinted tracks to sample for audio-anchored
/// resolution (Phase E) — a handful is plenty to agree on the artist.
const AUDIO_SAMPLE_LIMIT: i64 = 5;

/// Tunables sourced from [`crate::config::DiscographyConfig`].
#[derive(Debug, Clone)]
pub struct DiscographyCfg {
    pub match_threshold: u8,
    pub title_sim: f32,
    pub include_types: Vec<String>,
    pub sync_interval_secs: u64,
}

/// Result of a `sync_artist`: either a fresh report, or (when the artist can't
/// be confidently auto-matched) the candidate list for a manager to disambiguate.
#[derive(Debug)]
pub enum SyncOutcome {
    Report(DiscographyReport),
    NeedsResolution(Vec<ArtistCandidate>),
}

/// Library-wide coverage for the status endpoint / admin dashboard.
#[derive(Debug, Clone, Default)]
pub struct DiscographyStatus {
    pub enabled: bool,
    pub provider: String,
    pub artists_total: i64,
    pub matched: i64,
    pub unresolved: i64,
    pub ignored: i64,
}

/// Outcome of one background `run_pass`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscographyPassReport {
    pub synced: u64,
    pub skipped_fresh: u64,
    pub failed: u64,
    pub total: u64,
}

/// A local album plus its normalized title keys (title + aliases) for matching.
struct LocalAlbum {
    album: Album,
    keys: Vec<String>,
}

#[derive(Clone)]
pub struct DiscographyService {
    artists: Arc<dyn ArtistRepo>,
    albums: Arc<dyn AlbumRepo>,
    tracks: Arc<dyn TrackRepo>,
    aliases: Arc<dyn AliasRepo>,
    disco: Arc<dyn DiscographyRepo>,
    provider: Arc<dyn DiscographyProvider>,
    cfg: DiscographyCfg,
    /// Optional follower-notification hook (Phase D). `None` disables new-release
    /// alerts; the sync otherwise behaves identically.
    notifier: Option<Arc<dyn NewReleaseNotifier>>,
    /// Optional audio-anchored resolver (Phase E — AcoustID). `None` falls back
    /// to name-based resolution.
    audio_resolver: Option<Arc<dyn AudioResolver>>,
}

impl DiscographyService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        artists: Arc<dyn ArtistRepo>,
        albums: Arc<dyn AlbumRepo>,
        tracks: Arc<dyn TrackRepo>,
        aliases: Arc<dyn AliasRepo>,
        disco: Arc<dyn DiscographyRepo>,
        provider: Arc<dyn DiscographyProvider>,
        cfg: DiscographyCfg,
    ) -> Self {
        Self {
            artists,
            albums,
            tracks,
            aliases,
            disco,
            provider,
            cfg,
            notifier: None,
            audio_resolver: None,
        }
    }

    /// Wire in the follower-notification hook so a sync that detects a
    /// genuinely-new missing release alerts the artist's followers (Phase D).
    pub fn with_notifier(mut self, notifier: Option<Arc<dyn NewReleaseNotifier>>) -> Self {
        self.notifier = notifier;
        self
    }

    /// Wire in audio-anchored resolution (Phase E) — the artist is resolved from
    /// its tracks' Chromaprints (via AcoustID) before falling back to name search.
    pub fn with_audio_resolver(mut self, resolver: Option<Arc<dyn AudioResolver>>) -> Self {
        self.audio_resolver = resolver;
        self
    }

    // -----------------------------------------------------------------------
    // Sync
    // -----------------------------------------------------------------------

    /// Resolve (if needed) → fetch discography → diff against the library →
    /// persist the snapshot + filtered report. Returns the report, or the
    /// candidate list when the artist can't be confidently auto-matched.
    pub async fn sync_artist(&self, caller: &Identity, artist_id: Uuid) -> Result<SyncOutcome> {
        caller.require(PermissionLevel::Manager)?;
        let artist = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        let state = self.disco.get_state(artist_id).await?;

        // An explicitly-ignored artist is never reconciled.
        if state.as_ref().map(|s| s.match_status.as_str()) == Some("ignored") {
            let report = self
                .report(caller, artist_id)
                .await?
                .unwrap_or_else(|| empty_report(artist_id, self.provider.id()));
            return Ok(SyncOutcome::Report(report));
        }

        // Resolve the provider artist id (sticky once set — but only for the
        // *active* provider; a provider switch makes a stored id stale and
        // re-resolves).
        let resolved = state.as_ref().and_then(|s| {
            match (s.provider.as_deref(), s.provider_id.as_deref()) {
                (Some(p), Some(id)) if p == self.provider.id() => Some(id.to_string()),
                _ => None,
            }
        });
        let provider_artist_id = match resolved {
            Some(id) => id,
            None => match self.resolve_provider(&artist.name, artist_id).await? {
                Resolved::Id(id) => id,
                Resolved::Needs(candidates) => return Ok(SyncOutcome::NeedsResolution(candidates)),
            },
        };

        // Baseline for new-release detection (Phase D): the release-group ids
        // from the previous snapshot. `None` when never synced, so the first
        // sync only records a baseline and notifies nothing.
        let prev_ids = self.previous_release_ids(artist_id).await;

        // Fetch the discography and build the pre-ignore snapshot.
        let groups = self.provider.release_groups(&provider_artist_id).await?;
        let locals = self.load_local_albums(artist_id).await?;
        let mut used: HashSet<Uuid> = HashSet::new();
        let mut snap_groups = Vec::new();
        for rg in groups {
            if !self.cfg.include_types.iter().any(|t| t == &rg.album_type) {
                continue;
            }
            let rg_norm = normalize_title(&rg.title);
            let matched = self
                .match_album(&rg_norm, &locals, &used)
                .map(|la| la.album.clone());
            match matched {
                Some(album) => {
                    used.insert(album.id);
                    let missing = self.missing_tracks(&rg.provider_id, album.id).await;
                    snap_groups.push(SnapReleaseGroup {
                        provider_id: rg.provider_id,
                        title: rg.title,
                        album_type: rg.album_type,
                        year: rg.year,
                        matched_album_id: Some(album.id),
                        matched_album_title: Some(album.title),
                        missing_tracks: missing,
                    });
                }
                None => snap_groups.push(SnapReleaseGroup {
                    provider_id: rg.provider_id,
                    title: rg.title,
                    album_type: rg.album_type,
                    year: rg.year,
                    matched_album_id: None,
                    matched_album_title: None,
                    missing_tracks: Vec::new(),
                }),
            }
        }
        let snapshot = ProviderSnapshot {
            release_groups: snap_groups,
        };
        self.disco.touch_synced(artist_id).await?;
        let ignores = self.disco.list_ignores(artist_id).await?;
        let report = self
            .persist_filtered(artist_id, self.provider.id(), &snapshot, &ignores)
            .await?;
        self.notify_new_releases(artist_id, &snapshot, prev_ids.as_ref())
            .await;
        Ok(SyncOutcome::Report(report))
    }

    /// Resolve an artist to a provider id: audio-anchored first (Phase E — from
    /// the tracks' fingerprints), then the name-based confidence policy (§4.2).
    async fn resolve_provider(&self, name: &str, artist_id: Uuid) -> Result<Resolved> {
        // Phase E: try resolving from the artist's audio before guessing by name.
        if let Some(resolver) = &self.audio_resolver {
            let prints = self
                .disco
                .artist_chromaprints(artist_id, AUDIO_SAMPLE_LIMIT)
                .await
                .unwrap_or_default();
            if !prints.is_empty() {
                match resolver.resolve_artist(&prints).await {
                    Ok(Some(provider_id)) => {
                        self.disco
                            .upsert_state(
                                artist_id,
                                Some(self.provider.id()),
                                Some(&provider_id),
                                "matched",
                            )
                            .await?;
                        return Ok(Resolved::Id(provider_id));
                    }
                    Ok(None) => {} // abstained — fall back to name search
                    Err(e) => {
                        tracing::warn!(artist = %artist_id, error = %e, "discography: audio resolution failed")
                    }
                }
            }
        }

        let hints = self.hint_titles(artist_id).await;
        let candidates = self.provider.resolve_artist(name, &hints).await?;
        if candidates.is_empty() {
            self.disco
                .upsert_state(artist_id, None, None, "unresolved")
                .await?;
            return Ok(Resolved::Needs(vec![]));
        }
        let top = &candidates[0];
        let runner_up = candidates.get(1).map(|c| c.score).unwrap_or(0);
        let confident = top.score >= self.cfg.match_threshold
            && (candidates.len() == 1 || top.score.saturating_sub(runner_up) >= MATCH_MARGIN);
        if confident {
            self.disco
                .upsert_state(
                    artist_id,
                    Some(self.provider.id()),
                    Some(&top.provider_id),
                    "matched",
                )
                .await?;
            Ok(Resolved::Id(top.provider_id.clone()))
        } else {
            self.disco
                .upsert_state(artist_id, None, None, "unresolved")
                .await?;
            Ok(Resolved::Needs(candidates))
        }
    }

    /// Diff a matched release-group's canonical tracklist against a local album.
    async fn missing_tracks(&self, rg_id: &str, album_id: Uuid) -> Vec<SnapMissingTrack> {
        let provider_tracks = self.provider.tracklist(rg_id).await.unwrap_or_default();
        let local_keys = self.load_track_keys(album_id).await;
        let mut missing = Vec::new();
        for pt in provider_tracks {
            let key = normalize_title(&pt.title);
            if key.is_empty() {
                continue;
            }
            if !matches_any(&key, &local_keys, self.cfg.title_sim) {
                missing.push(SnapMissingTrack {
                    title: pt.title,
                    position: pt.position,
                    disc_no: pt.disc_no,
                    recording_id: pt.provider_id,
                    title_key: key,
                });
            }
        }
        missing
    }

    // -----------------------------------------------------------------------
    // Resolution management
    // -----------------------------------------------------------------------

    /// Provider artist candidates for the disambiguation UI.
    pub async fn candidates(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<Vec<ArtistCandidate>> {
        caller.require(PermissionLevel::Manager)?;
        let artist = self
            .artists
            .get(artist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("artist {artist_id}")))?;
        let hints = self.hint_titles(artist_id).await;
        self.provider.resolve_artist(&artist.name, &hints).await
    }

    /// Pin the artist ↔ provider match by hand (`Some(id)` → `manual`, tagged
    /// with the active provider), or set the artist to `ignored` (`None`) so
    /// it's excluded from reconciliation.
    pub async fn resolve(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        provider_id: Option<String>,
    ) -> Result<()> {
        caller.require(PermissionLevel::Manager)?;
        if self.artists.get(artist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("artist {artist_id}")));
        }
        match provider_id {
            Some(id) => {
                self.disco
                    .upsert_state(artist_id, Some(self.provider.id()), Some(&id), "manual")
                    .await
            }
            None => {
                self.disco
                    .upsert_state(artist_id, None, None, "ignored")
                    .await
            }
        }
    }

    /// The cached report (no network), or `None` if never synced.
    pub async fn report(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<Option<DiscographyReport>> {
        caller.require(PermissionLevel::Manager)?;
        let Some(stored) = self.disco.get_report(artist_id).await? else {
            return Ok(None);
        };
        Ok(Some(DiscographyReport {
            artist_id,
            provider: stored.provider,
            missing_releases: serde_json::from_str(&stored.missing_releases).unwrap_or_default(),
            incomplete_albums: serde_json::from_str(&stored.incomplete_albums).unwrap_or_default(),
            missing_release_count: stored.missing_release_count,
            incomplete_album_count: stored.incomplete_album_count,
            generated_at: stored.generated_at,
        }))
    }

    // -----------------------------------------------------------------------
    // Suppression (§4.7)
    // -----------------------------------------------------------------------

    /// Ignore a missing release (`scope=release`) or a missing track
    /// (`scope=track`). Idempotent. Returns the re-filtered report (network-free).
    pub async fn ignore(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        req: IgnoreRequest,
    ) -> Result<DiscographyReport> {
        caller.require(PermissionLevel::Manager)?;
        if req.scope != "release" && req.scope != "track" {
            return Err(AppError::InvalidArgument(
                "ignore scope must be 'release' or 'track'".into(),
            ));
        }
        if req.scope == "track" && req.recording_id.is_none() && req.title_key.is_none() {
            return Err(AppError::InvalidArgument(
                "a track ignore needs a recording_id or a title_key".into(),
            ));
        }
        self.disco
            .add_ignore(NewDiscographyIgnore {
                artist_id,
                scope: req.scope,
                release_group_id: req.release_group_id,
                recording_id: req.recording_id,
                title_key: req.title_key,
                label: req.label,
                created_by: caller.user_id(),
            })
            .await?;
        self.recompute(artist_id).await
    }

    /// Remove a suppression so the gap resurfaces. Returns the re-filtered report.
    pub async fn unignore(
        &self,
        caller: &Identity,
        artist_id: Uuid,
        ignore_id: Uuid,
    ) -> Result<DiscographyReport> {
        caller.require(PermissionLevel::Manager)?;
        // Verify the ignore exists and belongs to this artist (so a guessed id
        // can't remove another artist's suppression).
        match self.disco.get_ignore(ignore_id).await? {
            Some(ig) if ig.artist_id == artist_id => {}
            _ => return Err(AppError::NotFound(format!("ignore {ignore_id}"))),
        }
        self.disco.remove_ignore(artist_id, ignore_id).await?;
        self.recompute(artist_id).await
    }

    /// The artist's current suppression list (the "Ignored" management view).
    pub async fn list_ignores(
        &self,
        caller: &Identity,
        artist_id: Uuid,
    ) -> Result<Vec<DiscographyIgnore>> {
        caller.require(PermissionLevel::Manager)?;
        self.disco.list_ignores(artist_id).await
    }

    /// Re-run the ignore filter over the cached snapshot (no provider calls) and
    /// persist the fresh filtered report.
    async fn recompute(&self, artist_id: Uuid) -> Result<DiscographyReport> {
        let Some(stored) = self.disco.get_report(artist_id).await? else {
            return Ok(empty_report(artist_id, self.provider.id()));
        };
        let snapshot: ProviderSnapshot =
            serde_json::from_str(&stored.provider_snapshot).unwrap_or_default();
        let ignores = self.disco.list_ignores(artist_id).await?;
        self.persist_filtered(artist_id, &stored.provider, &snapshot, &ignores)
            .await
    }

    /// Apply the ignore filter to a snapshot, persist the result, return it.
    async fn persist_filtered(
        &self,
        artist_id: Uuid,
        provider: &str,
        snapshot: &ProviderSnapshot,
        ignores: &[DiscographyIgnore],
    ) -> Result<DiscographyReport> {
        let (missing_releases, incomplete_albums) = apply_ignores(snapshot, ignores);
        let missing_release_count = missing_releases.len() as i32;
        let incomplete_album_count = incomplete_albums.len() as i32;
        self.disco
            .upsert_report(NewStoredReport {
                artist_id,
                provider: provider.to_string(),
                missing_releases: serde_json::to_string(&missing_releases)
                    .unwrap_or_else(|_| "[]".to_string()),
                incomplete_albums: serde_json::to_string(&incomplete_albums)
                    .unwrap_or_else(|_| "[]".to_string()),
                provider_snapshot: serde_json::to_string(snapshot)
                    .unwrap_or_else(|_| "{}".to_string()),
                missing_release_count,
                incomplete_album_count,
            })
            .await?;
        Ok(DiscographyReport {
            artist_id,
            provider: provider.to_string(),
            missing_releases,
            incomplete_albums,
            missing_release_count,
            incomplete_album_count,
            generated_at: OffsetDateTime::now_utc(),
        })
    }

    // -----------------------------------------------------------------------
    // New-release notifications (Phase D)
    // -----------------------------------------------------------------------

    /// Release-group ids from the last stored snapshot, or `None` if the artist
    /// has never been synced (the baseline guard for new-release alerts).
    async fn previous_release_ids(&self, artist_id: Uuid) -> Option<HashSet<String>> {
        let stored = self.disco.get_report(artist_id).await.ok().flatten()?;
        let snap: ProviderSnapshot =
            serde_json::from_str(&stored.provider_snapshot).unwrap_or_default();
        Some(
            snap.release_groups
                .into_iter()
                .map(|rg| rg.provider_id)
                .collect(),
        )
    }

    /// Alert followers about genuinely-new missing releases: a release-group
    /// that is missing (not owned), absent from the previous snapshot, and
    /// recently released. No-op without a notifier, or on the first sync
    /// (`prev` is `None`) so the back-catalogue never spams. Best-effort —
    /// a notification failure never fails the sync.
    async fn notify_new_releases(
        &self,
        artist_id: Uuid,
        snapshot: &ProviderSnapshot,
        prev: Option<&HashSet<String>>,
    ) {
        let (Some(notifier), Some(prev)) = (self.notifier.as_ref(), prev) else {
            return;
        };
        let year_now = OffsetDateTime::now_utc().year();
        let fresh: Vec<&SnapReleaseGroup> = snapshot
            .release_groups
            .iter()
            .filter(|rg| {
                rg.matched_album_id.is_none()
                    && !prev.contains(&rg.provider_id)
                    && rg
                        .year
                        .is_some_and(|y| y >= year_now - NEW_RELEASE_RECENT_YEARS)
            })
            .collect();
        for rg in fresh.iter().take(NEW_RELEASE_NOTIFY_CAP) {
            if let Err(e) = notifier.notify(artist_id, &rg.title).await {
                tracing::warn!(artist = %artist_id, error = %e, "discography: new-release notify failed");
            }
        }
        if fresh.len() > NEW_RELEASE_NOTIFY_CAP {
            tracing::info!(
                artist = %artist_id,
                dropped = fresh.len() - NEW_RELEASE_NOTIFY_CAP,
                "discography: capped new-release notifications"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Status + background pass
    // -----------------------------------------------------------------------

    /// Library-wide coverage counts.
    pub async fn status(&self) -> DiscographyStatus {
        let states = self.disco.list_states().await.unwrap_or_default();
        let (mut matched, mut ignored) = (0i64, 0i64);
        for s in &states {
            match s.match_status.as_str() {
                "matched" | "manual" => matched += 1,
                "ignored" => ignored += 1,
                _ => {}
            }
        }
        let artists_total = self.artists.count().await.unwrap_or(0);
        DiscographyStatus {
            enabled: true,
            provider: self.provider.id().to_string(),
            artists_total,
            matched,
            unresolved: (artists_total - matched - ignored).max(0),
            ignored,
        }
    }

    /// Re-sync every matched/manual artist (rate-limited by the provider),
    /// skipping any synced within the freshness window. The background pass.
    pub async fn run_pass(&self) -> DiscographyPassReport {
        let states = self.disco.list_states().await.unwrap_or_default();
        let now = OffsetDateTime::now_utc();
        let mut report = DiscographyPassReport::default();
        for s in states {
            if s.match_status != "matched" && s.match_status != "manual" {
                continue;
            }
            report.total += 1;
            if let Some(synced) = s.synced_at {
                if now - synced < time::Duration::days(FRESHNESS_DAYS) {
                    report.skipped_fresh += 1;
                    continue;
                }
            }
            match self.sync_artist(&Identity::SecretKey, s.artist_id).await {
                Ok(_) => report.synced += 1,
                Err(e) => {
                    tracing::warn!(artist = %s.artist_id, error = %e, "discography pass: sync failed");
                    report.failed += 1;
                }
            }
        }
        tracing::info!(
            synced = report.synced,
            skipped_fresh = report.skipped_fresh,
            failed = report.failed,
            total = report.total,
            "discography pass complete"
        );
        report
    }

    /// Background sync-all on an interval (0 = manual only — no startup pass, to
    /// avoid hammering the provider at ~1 req/s on every boot).
    pub fn spawn_poller(self: &Arc<Self>, interval_secs: u64) {
        if interval_secs == 0 {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                this.run_pass().await;
            }
        });
    }

    // -----------------------------------------------------------------------
    // Matching helpers
    // -----------------------------------------------------------------------

    async fn hint_titles(&self, artist_id: Uuid) -> Vec<String> {
        self.albums
            .list_by_artist(artist_id)
            .await
            .map(|albums| albums.into_iter().take(5).map(|a| a.title).collect())
            .unwrap_or_default()
    }

    async fn load_local_albums(&self, artist_id: Uuid) -> Result<Vec<LocalAlbum>> {
        let albums = self.albums.list_by_artist(artist_id).await?;
        let mut out = Vec::with_capacity(albums.len());
        for album in albums {
            let mut keys = vec![normalize_title(&album.title)];
            if let Ok(aliases) = self.aliases.list_album_aliases(album.id).await {
                for a in aliases {
                    keys.push(normalize_title(&a.title));
                }
            }
            out.push(LocalAlbum { album, keys });
        }
        Ok(out)
    }

    async fn load_track_keys(&self, album_id: Uuid) -> Vec<String> {
        let tracks = match self.tracks.list_by_album(album_id).await {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let mut keys = Vec::new();
        for t in tracks {
            keys.push(normalize_title(&t.title));
            if let Ok(aliases) = self.aliases.list_track_aliases(t.id).await {
                for a in aliases {
                    keys.push(normalize_title(&a.title));
                }
            }
        }
        keys
    }

    /// Best local album for a normalized release-group title, skipping already-
    /// matched albums: a normalized-equality hit first, else the best fuzzy match
    /// at or above the configured threshold.
    fn match_album<'a>(
        &self,
        rg_norm: &str,
        locals: &'a [LocalAlbum],
        used: &HashSet<Uuid>,
    ) -> Option<&'a LocalAlbum> {
        for la in locals {
            if used.contains(&la.album.id) {
                continue;
            }
            if la.keys.iter().any(|k| k == rg_norm) {
                return Some(la);
            }
        }
        let mut best: Option<(&LocalAlbum, f32)> = None;
        for la in locals {
            if used.contains(&la.album.id) {
                continue;
            }
            let score = la
                .keys
                .iter()
                .map(|k| similarity(rg_norm, k))
                .fold(0.0f32, f32::max);
            if score >= self.cfg.title_sim && best.map(|(_, b)| score > b).unwrap_or(true) {
                best = Some((la, score));
            }
        }
        best.map(|(la, _)| la)
    }
}

/// A request to add a suppression (built from the REST/gRPC body). Provider ids
/// are strings (provider-agnostic — Phase D).
pub struct IgnoreRequest {
    pub scope: String,
    pub release_group_id: String,
    pub recording_id: Option<String>,
    pub title_key: Option<String>,
    pub label: String,
}

enum Resolved {
    Id(String),
    Needs(Vec<ArtistCandidate>),
}

fn empty_report(artist_id: Uuid, provider: &str) -> DiscographyReport {
    DiscographyReport {
        artist_id,
        provider: provider.to_string(),
        missing_releases: Vec::new(),
        incomplete_albums: Vec::new(),
        missing_release_count: 0,
        incomplete_album_count: 0,
        generated_at: OffsetDateTime::now_utc(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{
        Album, Artist, ArtistAlias, AlbumAlias, ArtistDiscoState, NewAlbum, NewAlbumAlias,
        NewArtist, NewArtistAlias, NewTrack, NewTrackAlias, StoredReport, Track, TrackAlias,
    };
    use crate::db::repo::{AliasRepo, AlbumRepo, ArtistRepo, DiscographyRepo, TrackIdPath, TrackRepo};
    use crate::services::discography::provider::{
        ProviderReleaseGroup, ProviderTrack,
    };
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ---- constructors -----------------------------------------------------

    fn mk_artist(name: &str) -> Artist {
        Artist {
            id: Uuid::new_v4(),
            name: name.to_string(),
            sort_name: None,
            image_path: None,
            storage_bytes: 0,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }
    fn mk_album(artist_id: Uuid, title: &str) -> Album {
        Album {
            id: Uuid::new_v4(),
            artist_id,
            title: title.to_string(),
            release_year: None,
            album_type: "album".to_string(),
            is_explicit: false,
            cover_path: None,
            storage_bytes: 0,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }
    fn mk_track(album_id: Uuid, artist_id: Uuid, title: &str) -> Track {
        Track {
            id: Uuid::new_v4(),
            album_id,
            artist_id,
            title: title.to_string(),
            track_no: None,
            disc_no: None,
            duration_ms: 1000,
            codec: "flac".to_string(),
            bitrate_kbps: None,
            file_path: "x".to_string(),
            file_size: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            metadata_json: "{}".to_string(),
            is_single_release: false,
            is_explicit: false,
            created_at: OffsetDateTime::now_utc(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    // ---- fake repos -------------------------------------------------------

    #[derive(Default)]
    struct FakeArtists {
        rows: Mutex<Vec<Artist>>,
    }
    #[async_trait]
    impl ArtistRepo for FakeArtists {
        async fn create(&self, _: NewArtist) -> Result<Artist> {
            unreachable!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Artist>> {
            Ok(self.rows.lock().unwrap().iter().find(|a| a.id == id).cloned())
        }
        async fn list(&self, _: i64, _: i64) -> Result<Vec<Artist>> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn count(&self) -> Result<i64> {
            Ok(self.rows.lock().unwrap().len() as i64)
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Artist>> {
            Ok(vec![])
        }
        async fn update(&self, _: Uuid, _: &str, _: Option<&str>) -> Result<Option<Artist>> {
            Ok(None)
        }
        async fn set_image(&self, _: Uuid, _: Option<&str>) -> Result<Option<Artist>> {
            Ok(None)
        }
        async fn all_image_paths(&self) -> Result<Vec<(Uuid, String)>> {
            Ok(vec![])
        }
        async fn find_by_name(&self, _: &str) -> Result<Option<Artist>> {
            Ok(None)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeAlbums {
        rows: Mutex<Vec<Album>>,
    }
    #[async_trait]
    impl AlbumRepo for FakeAlbums {
        async fn create(&self, _: NewAlbum) -> Result<Album> {
            unreachable!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Album>> {
            Ok(self.rows.lock().unwrap().iter().find(|a| a.id == id).cloned())
        }
        async fn list_by_artist(&self, artist_id: Uuid) -> Result<Vec<Album>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|a| a.artist_id == artist_id)
                .cloned()
                .collect())
        }
        async fn recent(&self, _: i64) -> Result<Vec<Album>> {
            Ok(vec![])
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Album>> {
            Ok(vec![])
        }
        async fn update(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i32>,
            _: Option<&str>,
        ) -> Result<Option<Album>> {
            Ok(None)
        }
        async fn set_album_type(&self, _: Uuid, _: &str) -> Result<Option<Album>> {
            Ok(None)
        }
        async fn recompute_explicit(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn find_by_artist_and_title(&self, _: Uuid, _: &str) -> Result<Option<Album>> {
            Ok(None)
        }
        async fn all_cover_paths(&self) -> Result<Vec<(Uuid, String)>> {
            Ok(vec![])
        }
        async fn reassign_artist(&self, _: Uuid, _: Uuid) -> Result<u64> {
            Ok(0)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTracks {
        rows: Mutex<Vec<Track>>,
    }
    #[async_trait]
    impl TrackRepo for FakeTracks {
        async fn create(&self, _: NewTrack) -> Result<Track> {
            unreachable!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Track>> {
            Ok(self.rows.lock().unwrap().iter().find(|t| t.id == id).cloned())
        }
        async fn list_by_album(&self, album_id: Uuid) -> Result<Vec<Track>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.album_id == album_id)
                .cloned()
                .collect())
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Track>> {
            Ok(vec![])
        }
        async fn update(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i32>,
            _: Option<i32>,
            _: &str,
        ) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn find_by_file_path(&self, _: &str) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn reassign_artist(&self, _: Uuid, _: Uuid) -> Result<u64> {
            Ok(0)
        }
        async fn reassign_album(&self, _: Uuid, _: Uuid) -> Result<u64> {
            Ok(0)
        }
        async fn set_album(&self, _: Uuid, _: Uuid) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn set_single_release(&self, _: Uuid, _: bool) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn set_explicit(&self, _: Uuid, _: bool) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn list_all_ids_paths(&self) -> Result<Vec<TrackIdPath>> {
            Ok(vec![])
        }
        async fn update_duration(&self, _: Uuid, _: i64) -> Result<Option<Track>> {
            Ok(None)
        }
        async fn update_file_props(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i32>,
            _: Option<i64>,
            _: Option<i32>,
            _: Option<i32>,
            _: Option<i32>,
        ) -> Result<Option<Track>> {
            Ok(None)
        }
    }

    /// Aliases aren't exercised by these tests — every list is empty.
    #[derive(Default)]
    struct FakeAliases;
    #[async_trait]
    impl AliasRepo for FakeAliases {
        async fn list_artist_aliases(&self, _: Uuid) -> Result<Vec<ArtistAlias>> {
            Ok(vec![])
        }
        async fn add_artist_alias(&self, _: NewArtistAlias) -> Result<ArtistAlias> {
            unreachable!()
        }
        async fn get_artist_alias(&self, _: Uuid) -> Result<Option<ArtistAlias>> {
            Ok(None)
        }
        async fn delete_artist_alias(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn set_primary_artist_alias(&self, _: Uuid, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn reassign_artist_aliases(&self, _: Uuid, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn list_album_aliases(&self, _: Uuid) -> Result<Vec<AlbumAlias>> {
            Ok(vec![])
        }
        async fn add_album_alias(&self, _: NewAlbumAlias) -> Result<AlbumAlias> {
            unreachable!()
        }
        async fn get_album_alias(&self, _: Uuid) -> Result<Option<AlbumAlias>> {
            Ok(None)
        }
        async fn delete_album_alias(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn set_primary_album_alias(&self, _: Uuid, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn reassign_album_aliases(&self, _: Uuid, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn list_track_aliases(&self, _: Uuid) -> Result<Vec<TrackAlias>> {
            Ok(vec![])
        }
        async fn add_track_alias(&self, _: NewTrackAlias) -> Result<TrackAlias> {
            unreachable!()
        }
        async fn get_track_alias(&self, _: Uuid) -> Result<Option<TrackAlias>> {
            Ok(None)
        }
        async fn delete_track_alias(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn set_primary_track_alias(&self, _: Uuid, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDisco {
        states: Mutex<HashMap<Uuid, ArtistDiscoState>>,
        reports: Mutex<HashMap<Uuid, StoredReport>>,
        ignores: Mutex<Vec<DiscographyIgnore>>,
    }
    #[async_trait]
    impl DiscographyRepo for FakeDisco {
        async fn get_state(&self, artist_id: Uuid) -> Result<Option<ArtistDiscoState>> {
            Ok(self.states.lock().unwrap().get(&artist_id).cloned())
        }
        async fn upsert_state(
            &self,
            artist_id: Uuid,
            provider: Option<&str>,
            provider_id: Option<&str>,
            match_status: &str,
        ) -> Result<()> {
            let mut g = self.states.lock().unwrap();
            let synced_at = g.get(&artist_id).and_then(|s| s.synced_at);
            g.insert(
                artist_id,
                ArtistDiscoState {
                    artist_id,
                    provider: provider.map(str::to_string),
                    provider_id: provider_id.map(str::to_string),
                    match_status: match_status.to_string(),
                    synced_at,
                },
            );
            Ok(())
        }
        async fn touch_synced(&self, artist_id: Uuid) -> Result<()> {
            let mut g = self.states.lock().unwrap();
            let e = g.entry(artist_id).or_insert(ArtistDiscoState {
                artist_id,
                provider: None,
                provider_id: None,
                match_status: "unresolved".to_string(),
                synced_at: None,
            });
            e.synced_at = Some(OffsetDateTime::now_utc());
            Ok(())
        }
        async fn list_states(&self) -> Result<Vec<ArtistDiscoState>> {
            Ok(self.states.lock().unwrap().values().cloned().collect())
        }
        async fn artist_chromaprints(
            &self,
            _artist_id: Uuid,
            _limit: i64,
        ) -> Result<Vec<crate::db::models::TrackFingerprint>> {
            Ok(vec![])
        }
        async fn upsert_report(&self, r: NewStoredReport) -> Result<()> {
            self.reports.lock().unwrap().insert(
                r.artist_id,
                StoredReport {
                    provider: r.provider,
                    missing_releases: r.missing_releases,
                    incomplete_albums: r.incomplete_albums,
                    provider_snapshot: r.provider_snapshot,
                    missing_release_count: r.missing_release_count,
                    incomplete_album_count: r.incomplete_album_count,
                    generated_at: OffsetDateTime::now_utc(),
                },
            );
            Ok(())
        }
        async fn get_report(&self, artist_id: Uuid) -> Result<Option<StoredReport>> {
            Ok(self.reports.lock().unwrap().get(&artist_id).cloned())
        }
        async fn add_ignore(&self, new: NewDiscographyIgnore) -> Result<()> {
            let mut g = self.ignores.lock().unwrap();
            // Idempotent: skip an equivalent existing entry.
            let dup = g.iter().any(|i| {
                i.artist_id == new.artist_id
                    && i.scope == new.scope
                    && i.release_group_id == new.release_group_id
                    && i.recording_id == new.recording_id
                    && i.title_key == new.title_key
            });
            if !dup {
                g.push(DiscographyIgnore {
                    id: Uuid::new_v4(),
                    artist_id: new.artist_id,
                    scope: new.scope,
                    release_group_id: new.release_group_id,
                    recording_id: new.recording_id,
                    title_key: new.title_key,
                    label: new.label,
                    created_at: OffsetDateTime::now_utc(),
                });
            }
            Ok(())
        }
        async fn get_ignore(&self, id: Uuid) -> Result<Option<DiscographyIgnore>> {
            Ok(self.ignores.lock().unwrap().iter().find(|i| i.id == id).cloned())
        }
        async fn remove_ignore(&self, artist_id: Uuid, id: Uuid) -> Result<()> {
            self.ignores
                .lock()
                .unwrap()
                .retain(|i| !(i.id == id && i.artist_id == artist_id));
            Ok(())
        }
        async fn list_ignores(&self, artist_id: Uuid) -> Result<Vec<DiscographyIgnore>> {
            Ok(self
                .ignores
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i.artist_id == artist_id)
                .cloned()
                .collect())
        }
    }

    // ---- fake provider (counts calls) -------------------------------------

    #[derive(Default)]
    struct FakeProvider {
        candidates: Vec<ArtistCandidate>,
        // Mutable so a test can add a "new release" between syncs.
        groups: Mutex<Vec<ProviderReleaseGroup>>,
        tracks: Vec<ProviderTrack>,
        resolve_calls: AtomicUsize,
        rg_calls: AtomicUsize,
        track_calls: AtomicUsize,
    }
    #[async_trait]
    impl DiscographyProvider for FakeProvider {
        fn id(&self) -> &str {
            "fake"
        }
        async fn resolve_artist(&self, _: &str, _: &[String]) -> Result<Vec<ArtistCandidate>> {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.candidates.clone())
        }
        async fn release_groups(&self, _: &str) -> Result<Vec<ProviderReleaseGroup>> {
            self.rg_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.groups.lock().unwrap().clone())
        }
        async fn tracklist(&self, _: &str) -> Result<Vec<ProviderTrack>> {
            self.track_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.tracks.clone())
        }
    }

    /// Counting notifier — records each new-release alert the sync fires.
    #[derive(Default)]
    struct FakeNotifier {
        count: AtomicUsize,
        titles: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl NewReleaseNotifier for FakeNotifier {
        async fn notify(&self, _artist_id: Uuid, title: &str) -> Result<u64> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.titles.lock().unwrap().push(title.to_string());
            Ok(1)
        }
    }

    // ---- harness ----------------------------------------------------------

    struct Ctx {
        svc: DiscographyService,
        provider: Arc<FakeProvider>,
        disco: Arc<FakeDisco>,
        notifier: Arc<FakeNotifier>,
        artist_id: Uuid,
        animals_rg: String,
    }

    /// Library: one artist "Pink Floyd" owning "The Wall" (with only "Mother").
    /// Provider: candidate + two release-groups ("The Wall" owned, "Animals"
    /// missing) + a "The Wall" tracklist that adds "Another Brick in the Wall".
    fn make(candidates: Vec<ArtistCandidate>) -> Ctx {
        let artist = mk_artist("Pink Floyd");
        let artist_id = artist.id;
        let wall = mk_album(artist_id, "The Wall");
        let wall_id = wall.id;
        let mother = mk_track(wall_id, artist_id, "Mother");

        let artists = Arc::new(FakeArtists {
            rows: Mutex::new(vec![artist]),
        });
        let albums = Arc::new(FakeAlbums {
            rows: Mutex::new(vec![wall]),
        });
        let tracks = Arc::new(FakeTracks {
            rows: Mutex::new(vec![mother]),
        });
        let aliases = Arc::new(FakeAliases);
        let disco = Arc::new(FakeDisco::default());

        let wall_rg = Uuid::new_v4().to_string();
        let animals_rg = Uuid::new_v4().to_string();
        let provider = Arc::new(FakeProvider {
            candidates,
            groups: Mutex::new(vec![
                ProviderReleaseGroup {
                    provider_id: wall_rg,
                    title: "The Wall".to_string(),
                    album_type: "album".to_string(),
                    year: Some(1979),
                },
                ProviderReleaseGroup {
                    provider_id: animals_rg.clone(),
                    title: "Animals".to_string(),
                    album_type: "album".to_string(),
                    year: Some(1977),
                },
            ]),
            tracks: vec![
                ProviderTrack {
                    provider_id: None,
                    position: Some(1),
                    disc_no: Some(1),
                    title: "Mother".to_string(),
                },
                ProviderTrack {
                    provider_id: None,
                    position: Some(2),
                    disc_no: Some(1),
                    title: "Another Brick in the Wall".to_string(),
                },
            ],
            ..Default::default()
        });

        let cfg = DiscographyCfg {
            match_threshold: 90,
            title_sim: 0.9,
            include_types: vec!["album".into(), "ep".into(), "single".into(), "live".into()],
            sync_interval_secs: 0,
        };
        let notifier = Arc::new(FakeNotifier::default());
        let svc = DiscographyService::new(
            artists,
            albums,
            tracks,
            aliases,
            disco.clone(),
            provider.clone(),
            cfg,
        )
        .with_notifier(Some(notifier.clone() as Arc<dyn NewReleaseNotifier>));
        Ctx {
            svc,
            provider,
            disco,
            notifier,
            artist_id,
            animals_rg,
        }
    }

    fn candidate(id: &str, score: u8) -> ArtistCandidate {
        ArtistCandidate {
            provider_id: id.to_string(),
            name: "Pink Floyd".to_string(),
            disambiguation: None,
            score,
        }
    }

    fn mgr() -> Identity {
        Identity::SecretKey // effective Admin ⇒ satisfies Manager
    }

    #[tokio::test]
    async fn auto_accepts_and_builds_report() {
        let ctx = make(vec![candidate(&Uuid::new_v4().to_string(), 100)]);
        let out = ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        let report = match out {
            SyncOutcome::Report(r) => r,
            SyncOutcome::NeedsResolution(_) => panic!("expected a report"),
        };
        // "Animals" is missing; "The Wall" is owned but missing one track.
        assert_eq!(report.missing_releases.len(), 1);
        assert_eq!(report.missing_releases[0].title, "Animals");
        assert_eq!(report.incomplete_albums.len(), 1);
        assert_eq!(report.incomplete_albums[0].missing_tracks.len(), 1);
        assert_eq!(
            report.incomplete_albums[0].missing_tracks[0].title,
            "Another Brick in the Wall"
        );
        // State is now matched.
        let st = ctx.disco.get_state(ctx.artist_id).await.unwrap().unwrap();
        assert_eq!(st.match_status, "matched");
        assert!(st.provider_id.is_some());
    }

    #[tokio::test]
    async fn ambiguous_top_two_need_resolution() {
        // 95 vs 93 → within the 5-point margin ⇒ not confident.
        let ctx = make(vec![
            candidate(&Uuid::new_v4().to_string(), 95),
            candidate(&Uuid::new_v4().to_string(), 93),
        ]);
        let out = ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        assert!(matches!(out, SyncOutcome::NeedsResolution(_)));
        // Didn't fetch the discography.
        assert_eq!(ctx.provider.rg_calls.load(Ordering::SeqCst), 0);
        let st = ctx.disco.get_state(ctx.artist_id).await.unwrap().unwrap();
        assert_eq!(st.match_status, "unresolved");
    }

    #[tokio::test]
    async fn user_caller_is_rejected() {
        let ctx = make(vec![candidate(&Uuid::new_v4().to_string(), 100)]);
        let user = Identity::User {
            id: Uuid::new_v4(),
            username: "u".to_string(),
            level: PermissionLevel::User,
        };
        let err = ctx.svc.sync_artist(&user, ctx.artist_id).await.unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));
        let err = ctx.svc.report(&user, ctx.artist_id).await.unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));
    }

    #[tokio::test]
    async fn ignore_and_unignore_recompute_without_provider_calls() {
        let ctx = make(vec![candidate(&Uuid::new_v4().to_string(), 100)]);
        ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        let calls_after_sync = (
            ctx.provider.resolve_calls.load(Ordering::SeqCst),
            ctx.provider.rg_calls.load(Ordering::SeqCst),
            ctx.provider.track_calls.load(Ordering::SeqCst),
        );

        // Ignore the missing "Animals" release → it drops out of the report.
        let report = ctx
            .svc
            .ignore(
                &mgr(),
                ctx.artist_id,
                IgnoreRequest {
                    scope: "release".to_string(),
                    release_group_id: ctx.animals_rg.clone(),
                    recording_id: None,
                    title_key: None,
                    label: "Animals".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(report.missing_releases.len(), 0);
        // The incomplete album is untouched.
        assert_eq!(report.incomplete_albums.len(), 1);

        // Un-ignore → the release resurfaces.
        let id = ctx.disco.list_ignores(ctx.artist_id).await.unwrap()[0].id;
        let report = ctx.svc.unignore(&mgr(), ctx.artist_id, id).await.unwrap();
        assert_eq!(report.missing_releases.len(), 1);

        // Crucially: ignore/unignore hit the cached snapshot, not the provider.
        let calls_now = (
            ctx.provider.resolve_calls.load(Ordering::SeqCst),
            ctx.provider.rg_calls.load(Ordering::SeqCst),
            ctx.provider.track_calls.load(Ordering::SeqCst),
        );
        assert_eq!(calls_after_sync, calls_now, "suppression must not call the provider");
    }

    #[tokio::test]
    async fn notifies_only_new_recent_missing_releases() {
        let ctx = make(vec![candidate(&Uuid::new_v4().to_string(), 100)]);
        // First sync = baseline. "Animals" is missing but it's the
        // back-catalogue, so nothing is announced.
        ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        assert_eq!(ctx.notifier.count.load(Ordering::SeqCst), 0);

        // A brand-new release (this year) appears on the provider → the next
        // sync announces exactly it.
        let year = OffsetDateTime::now_utc().year();
        ctx.provider.groups.lock().unwrap().push(ProviderReleaseGroup {
            provider_id: Uuid::new_v4().to_string(),
            title: "Brand New LP".to_string(),
            album_type: "album".to_string(),
            year: Some(year),
        });
        ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        assert_eq!(ctx.notifier.count.load(Ordering::SeqCst), 1);
        assert_eq!(
            ctx.notifier.titles.lock().unwrap().as_slice(),
            &["Brand New LP".to_string()]
        );
    }

    #[tokio::test]
    async fn does_not_notify_for_newly_cataloged_old_releases() {
        let ctx = make(vec![candidate(&Uuid::new_v4().to_string(), 100)]);
        ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        // A newly-*cataloged* but decades-old release must not masquerade as new.
        ctx.provider.groups.lock().unwrap().push(ProviderReleaseGroup {
            provider_id: Uuid::new_v4().to_string(),
            title: "Old Reissue".to_string(),
            album_type: "album".to_string(),
            year: Some(1970),
        });
        ctx.svc.sync_artist(&mgr(), ctx.artist_id).await.unwrap();
        assert_eq!(ctx.notifier.count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dominant_artist_needs_agreement_or_high_confidence() {
        let a = "artist-a".to_string();
        let b = "artist-b".to_string();
        // Two tracks agree on A (even with a feature on the second) → resolved.
        assert_eq!(
            dominant_artist(&[(vec![a.clone()], 0.8), (vec![a.clone(), b.clone()], 0.7)]),
            Some(a.clone())
        );
        // A lone, low-confidence track abstains.
        assert_eq!(dominant_artist(&[(vec![a.clone()], 0.5)]), None);
        // A lone, high-confidence single-artist track resolves.
        assert_eq!(dominant_artist(&[(vec![a.clone()], 0.95)]), Some(a.clone()));
        // No results at all → None.
        assert_eq!(dominant_artist(&[(vec![], 0.99)]), None);
        // Two tracks that disagree, neither strong → None.
        assert_eq!(
            dominant_artist(&[(vec![a.clone()], 0.6), (vec![b.clone()], 0.6)]),
            None
        );
    }
}
