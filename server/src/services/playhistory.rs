//! Play history (Phase 11).
//!
//! Records a per-user "play" event each time the client decides a track has
//! been listened to (its rule — e.g. ≥30 s OR ≥50 % of the track). The events
//! drive "recently played", listening stats (top tracks/artists + totals over
//! a window), and the behavioral recommendation engine.
//!
//! Any authed *user* may record/read their own history (the `SECRET_KEY`
//! identity has no `user_id`, so it is rejected — there's no user to own it).
//! Unlike catalog mutations, play events are **private telemetry and are not
//! audited** — auditing every play would swamp the audit log without recording
//! a change to the library.

use std::sync::Arc;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{NewPlayEvent, PermissionLevel, PlayEvent};
use crate::db::repo::{
    ArtistPlayStat, ArtistRepo, PlayHistoryRepo, PlayTotals, TrackPlayStat, TrackRepo,
};
use crate::error::{AppError, Result};

const MAX_PAGE_LIMIT: i64 = 200;
const DEFAULT_PAGE_LIMIT: i64 = 50;
const MAX_STATS_LIMIT: i64 = 100;
const DEFAULT_STATS_LIMIT: i64 = 20;
/// Cap on events accepted in a single `record` call, so a flushed offline
/// backlog can't post an unbounded batch.
const MAX_BATCH: usize = 500;

/// A play posted by the client. The denormalized display fields are resolved
/// server-side from `track_id`, so the client only supplies these.
#[derive(Debug, Clone)]
pub struct PlayInput {
    pub track_id: Uuid,
    pub ms_played: i64,
    pub completed: bool,
    /// When the play happened. `None` → the server stamps receipt time. Set by
    /// the client when flushing offline plays so they keep their real time.
    pub played_at: Option<OffsetDateTime>,
}

/// Aggregate listening stats over a window.
#[derive(Debug, Clone)]
pub struct ListeningStats {
    pub top_tracks: Vec<TrackPlayStat>,
    pub top_artists: Vec<ArtistPlayStat>,
    pub totals: PlayTotals,
}

#[derive(Clone)]
pub struct PlayHistoryService {
    pub plays: Arc<dyn PlayHistoryRepo>,
    /// Tracks are read to resolve the denormalized title/album/artist of a play.
    pub tracks: Arc<dyn TrackRepo>,
    /// Artists are read for the denormalized artist name on a play.
    pub artists: Arc<dyn ArtistRepo>,
}

impl PlayHistoryService {
    pub fn new(
        plays: Arc<dyn PlayHistoryRepo>,
        tracks: Arc<dyn TrackRepo>,
        artists: Arc<dyn ArtistRepo>,
    ) -> Self {
        Self {
            plays,
            tracks,
            artists,
        }
    }

    /// Record a batch of plays. Each event's `track_id` is resolved to the
    /// current title/artist/album server-side; events whose track no longer
    /// exists are silently skipped. Returns the number of events recorded. Any
    /// authed user; `SECRET_KEY` rejected.
    pub async fn record(&self, caller: &Identity, events: &[PlayInput]) -> Result<u64> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        if events.len() > MAX_BATCH {
            return Err(AppError::InvalidArgument(format!(
                "too many events in one batch (max {MAX_BATCH})"
            )));
        }

        let mut rows: Vec<NewPlayEvent> = Vec::with_capacity(events.len());
        for ev in events {
            // Resolve the track; drop the event if the track is gone (a stale
            // offline backlog can reference a since-deleted track).
            let Some(track) = self.tracks.get(ev.track_id).await? else {
                continue;
            };
            let artist_name = match self.artists.get(track.artist_id).await? {
                Some(a) => a.name,
                None => "Unknown Artist".to_string(),
            };
            rows.push(NewPlayEvent {
                user_id,
                track_id: track.id,
                artist_id: track.artist_id,
                album_id: track.album_id,
                track_title: track.title,
                artist_name,
                ms_played: ev.ms_played.max(0),
                completed: ev.completed,
                played_at: ev.played_at,
            });
        }
        self.plays.record_many(&rows).await
    }

    /// A page of the caller's plays, newest first. `SECRET_KEY` rejected.
    pub async fn recent(
        &self,
        caller: &Identity,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<PlayEvent>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let (limit, offset) = paginate(limit, offset);
        self.plays.recent(user_id, limit, offset).await
    }

    /// Aggregate listening stats for the caller over the last `window_days`
    /// (`None`/0 = all time). `limit` bounds the top-N lists. `SECRET_KEY`
    /// rejected.
    pub async fn stats(
        &self,
        caller: &Identity,
        window_days: Option<i64>,
        limit: Option<i64>,
    ) -> Result<ListeningStats> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let since = window_start(window_days);
        let limit = limit
            .unwrap_or(DEFAULT_STATS_LIMIT)
            .clamp(1, MAX_STATS_LIMIT);
        let top_tracks = self.plays.top_tracks(user_id, since, limit).await?;
        let top_artists = self.plays.top_artists(user_id, since, limit).await?;
        let totals = self.plays.totals(user_id, since).await?;
        Ok(ListeningStats {
            top_tracks,
            top_artists,
            totals,
        })
    }

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity has no user to own play history; log in as a user".into(),
            )
        })
    }
}

fn paginate(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
    let offset = offset.unwrap_or(0).max(0);
    (limit, offset)
}

/// Window start for a stats query: `now - window_days`, or the Unix epoch
/// (effectively "all time") when `window_days` is `None`/≤0.
fn window_start(window_days: Option<i64>) -> OffsetDateTime {
    match window_days {
        Some(d) if d > 0 => OffsetDateTime::now_utc() - Duration::days(d),
        _ => OffsetDateTime::UNIX_EPOCH,
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests against in-memory fakes (no live Postgres). They validate the
    //! permission rules (`SECRET_KEY` rejection), track hydration + skip-on-
    //! missing, the recording batch, and the recent/stats reads.

    use super::*;
    use crate::db::models::{Artist, NewArtist, NewTrack, Track};
    use crate::db::repo::TrackIdPath;
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // ---- Play-history fake ----
    #[derive(Default)]
    struct FakePlays {
        rows: Mutex<Vec<PlayEvent>>,
    }
    #[async_trait]
    impl PlayHistoryRepo for FakePlays {
        async fn record_many(&self, items: &[NewPlayEvent]) -> Result<u64> {
            let mut g = self.rows.lock().unwrap();
            for it in items {
                g.push(PlayEvent {
                    id: Uuid::new_v4(),
                    user_id: it.user_id,
                    track_id: Some(it.track_id),
                    artist_id: Some(it.artist_id),
                    album_id: Some(it.album_id),
                    track_title: it.track_title.clone(),
                    artist_name: it.artist_name.clone(),
                    ms_played: it.ms_played,
                    completed: it.completed,
                    played_at: it.played_at.unwrap_or_else(now),
                });
            }
            Ok(items.len() as u64)
        }
        async fn recent(&self, user_id: Uuid, limit: i64, offset: i64) -> Result<Vec<PlayEvent>> {
            let mut rows: Vec<PlayEvent> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.user_id == user_id)
                .cloned()
                .collect();
            rows.sort_by(|a, b| b.played_at.cmp(&a.played_at));
            Ok(rows
                .into_iter()
                .skip(offset.max(0) as usize)
                .take(limit.max(0) as usize)
                .collect())
        }
        async fn top_tracks(
            &self,
            user_id: Uuid,
            since: OffsetDateTime,
            limit: i64,
        ) -> Result<Vec<TrackPlayStat>> {
            use std::collections::HashMap;
            let g = self.rows.lock().unwrap();
            let mut counts: HashMap<(Option<Uuid>, String, String), i64> = HashMap::new();
            for p in g
                .iter()
                .filter(|p| p.user_id == user_id && p.played_at >= since)
            {
                *counts
                    .entry((p.track_id, p.track_title.clone(), p.artist_name.clone()))
                    .or_default() += 1;
            }
            let mut out: Vec<TrackPlayStat> = counts
                .into_iter()
                .map(|((track_id, track_title, artist_name), plays)| TrackPlayStat {
                    track_id,
                    track_title,
                    artist_name,
                    plays,
                })
                .collect();
            out.sort_by(|a, b| b.plays.cmp(&a.plays).then(a.track_title.cmp(&b.track_title)));
            out.truncate(limit.max(0) as usize);
            Ok(out)
        }
        async fn top_artists(
            &self,
            user_id: Uuid,
            since: OffsetDateTime,
            limit: i64,
        ) -> Result<Vec<ArtistPlayStat>> {
            use std::collections::HashMap;
            let g = self.rows.lock().unwrap();
            let mut counts: HashMap<(Option<Uuid>, String), i64> = HashMap::new();
            for p in g
                .iter()
                .filter(|p| p.user_id == user_id && p.played_at >= since)
            {
                *counts
                    .entry((p.artist_id, p.artist_name.clone()))
                    .or_default() += 1;
            }
            let mut out: Vec<ArtistPlayStat> = counts
                .into_iter()
                .map(|((artist_id, artist_name), plays)| ArtistPlayStat {
                    artist_id,
                    artist_name,
                    plays,
                })
                .collect();
            out.sort_by(|a, b| b.plays.cmp(&a.plays).then(a.artist_name.cmp(&b.artist_name)));
            out.truncate(limit.max(0) as usize);
            Ok(out)
        }
        async fn totals(&self, user_id: Uuid, since: OffsetDateTime) -> Result<PlayTotals> {
            let g = self.rows.lock().unwrap();
            let mut total_plays = 0;
            let mut total_ms = 0;
            for p in g
                .iter()
                .filter(|p| p.user_id == user_id && p.played_at >= since)
            {
                total_plays += 1;
                total_ms += p.ms_played;
            }
            Ok(PlayTotals {
                total_plays,
                total_ms,
            })
        }
        async fn play_count(&self, user_id: Uuid, track_id: Uuid) -> Result<i64> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.user_id == user_id && p.track_id == Some(track_id))
                .count() as i64)
        }
    }

    // ---- Tracks fake (only get/create exercised) ----
    #[derive(Default)]
    struct FakeTracks {
        rows: Mutex<Vec<Track>>,
    }
    impl FakeTracks {
        fn insert(&self, artist_id: Uuid, album_id: Uuid, title: &str) -> Track {
            let t = Track {
                id: Uuid::new_v4(),
                album_id,
                artist_id,
                title: title.to_string(),
                track_no: None,
                disc_no: None,
                duration_ms: 200_000,
                codec: "flac".into(),
                bitrate_kbps: None,
                file_path: format!("/lib/{title}.flac"),
                file_size: None,
                sample_rate_hz: None,
                bit_depth: None,
                channels: None,
                metadata_json: "{}".into(),
                is_single_release: false,
                created_at: now(),
                updated_at: now(),
            };
            self.rows.lock().unwrap().push(t.clone());
            t
        }
    }
    #[async_trait]
    impl TrackRepo for FakeTracks {
        async fn create(&self, _: NewTrack) -> Result<Track> {
            unreachable!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Track>> {
            Ok(self.rows.lock().unwrap().iter().find(|t| t.id == id).cloned())
        }
        async fn list_by_album(&self, _: Uuid) -> Result<Vec<Track>> {
            Ok(vec![])
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

    // ---- Artists fake (only get/insert exercised) ----
    #[derive(Default)]
    struct FakeArtists {
        rows: Mutex<Vec<Artist>>,
    }
    impl FakeArtists {
        fn insert(&self, name: &str) -> Artist {
            let a = Artist {
                id: Uuid::new_v4(),
                name: name.to_string(),
                sort_name: None,
                image_path: None,
                storage_bytes: 0,
                created_at: now(),
                updated_at: now(),
            };
            self.rows.lock().unwrap().push(a.clone());
            a
        }
    }
    #[async_trait]
    impl ArtistRepo for FakeArtists {
        async fn create(&self, new: NewArtist) -> Result<Artist> {
            Ok(self.insert(&new.name))
        }
        async fn get(&self, id: Uuid) -> Result<Option<Artist>> {
            Ok(self.rows.lock().unwrap().iter().find(|a| a.id == id).cloned())
        }
        async fn list(&self, _: i64, _: i64) -> Result<Vec<Artist>> {
            Ok(vec![])
        }
        async fn count(&self) -> Result<i64> {
            Ok(0)
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

    fn make_service() -> (PlayHistoryService, Arc<FakeTracks>, Arc<FakeArtists>) {
        let plays = Arc::new(FakePlays::default());
        let tracks = Arc::new(FakeTracks::default());
        let artists = Arc::new(FakeArtists::default());
        let svc = PlayHistoryService::new(plays, tracks.clone(), artists.clone());
        (svc, tracks, artists)
    }

    fn user() -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level: PermissionLevel::User,
        }
    }

    fn play(track_id: Uuid, ms: i64, completed: bool) -> PlayInput {
        PlayInput {
            track_id,
            ms_played: ms,
            completed,
            played_at: None,
        }
    }

    #[tokio::test]
    async fn record_hydrates_and_lists_recent() {
        let (svc, tracks, artists) = make_service();
        let me = user();
        let artist = artists.insert("LE SSERAFIM");
        let album_id = Uuid::new_v4();
        let t1 = tracks.insert(artist.id, album_id, "ANTIFRAGILE");
        let t2 = tracks.insert(artist.id, album_id, "UNFORGIVEN");

        let n = svc
            .record(&me, &[play(t1.id, 200_000, true), play(t2.id, 30_000, false)])
            .await
            .unwrap();
        assert_eq!(n, 2);

        let recent = svc.recent(&me, None, None).await.unwrap();
        assert_eq!(recent.len(), 2);
        // Denormalized fields were resolved from the track + artist.
        let titles: Vec<&str> = recent.iter().map(|p| p.track_title.as_str()).collect();
        assert!(titles.contains(&"ANTIFRAGILE"));
        assert!(recent.iter().all(|p| p.artist_name == "LE SSERAFIM"));
        assert!(recent.iter().all(|p| p.artist_id == Some(artist.id)));
    }

    #[tokio::test]
    async fn record_skips_missing_tracks() {
        let (svc, tracks, artists) = make_service();
        let me = user();
        let artist = artists.insert("a");
        let t1 = tracks.insert(artist.id, Uuid::new_v4(), "real");
        // One real track + one that doesn't exist → only the real one recorded.
        let n = svc
            .record(&me, &[play(t1.id, 1000, false), play(Uuid::new_v4(), 1000, false)])
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(svc.recent(&me, None, None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn secret_key_cannot_record_or_read() {
        let (svc, ..) = make_service();
        let err = svc
            .record(&Identity::SecretKey, &[play(Uuid::new_v4(), 1, false)])
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        let err = svc.recent(&Identity::SecretKey, None, None).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        let err = svc.stats(&Identity::SecretKey, None, None).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn stats_rank_top_tracks_artists_and_totals() {
        let (svc, tracks, artists) = make_service();
        let me = user();
        let a1 = artists.insert("Artist One");
        let a2 = artists.insert("Artist Two");
        let album = Uuid::new_v4();
        let hit = tracks.insert(a1.id, album, "Hit");
        let other = tracks.insert(a2.id, album, "Other");

        // 3 plays of the hit, 1 of the other.
        svc.record(
            &me,
            &[
                play(hit.id, 100, true),
                play(hit.id, 100, true),
                play(hit.id, 100, true),
                play(other.id, 50, false),
            ],
        )
        .await
        .unwrap();

        let stats = svc.stats(&me, None, None).await.unwrap();
        assert_eq!(stats.totals.total_plays, 4);
        assert_eq!(stats.totals.total_ms, 350);
        assert_eq!(stats.top_tracks[0].track_title, "Hit");
        assert_eq!(stats.top_tracks[0].plays, 3);
        assert_eq!(stats.top_artists[0].artist_name, "Artist One");
        assert_eq!(stats.top_artists[0].plays, 3);
    }

    #[tokio::test]
    async fn batch_over_cap_is_rejected() {
        let (svc, tracks, artists) = make_service();
        let me = user();
        let artist = artists.insert("a");
        let t = tracks.insert(artist.id, Uuid::new_v4(), "t");
        let batch: Vec<PlayInput> = (0..(MAX_BATCH + 1)).map(|_| play(t.id, 1, false)).collect();
        let err = svc.record(&me, &batch).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }
}
