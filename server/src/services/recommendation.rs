//! Recommendations / Discover (Phase 11).
//!
//! A **behavioral** recommender built from the user's play history + favorites +
//! the library graph — all cheap reads over existing repos, no new tables and
//! no stored model. (True acoustic-fingerprint similarity is a later phase; see
//! the Tier-1 plan.)
//!
//! [`get_home`](RecommendationService::get_home) returns named album shelves
//! (only the non-empty ones, so a fresh account still gets "Recently added").
//! [`get_radio`](RecommendationService::get_radio) returns a track queue seeded
//! from an artist or album. Home is personalized (per-user; `SECRET_KEY`
//! rejected); radio is library-derived (any authed identity).

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use time::OffsetDateTime;

use crate::auth::Identity;
use crate::db::models::{Album, FavoriteKind, PermissionLevel, Track};
use crate::db::repo::{AlbumRepo, ArtistRepo, FavoriteRepo, PlayHistoryRepo, TrackRepo};
use crate::error::{AppError, Result};
use crate::services::fingerprint::SimilarityIndex;

/// Albums per home shelf.
const SECTION_LIMIT: usize = 12;
/// Top-artist / favorite candidates considered for the "from artists" shelf.
const ARTIST_CANDIDATES: i64 = 10;
/// Recent plays scanned to derive the "jump back in" shelf.
const RECENT_PLAYS_SCAN: i64 = 60;
/// Max tracks returned by a radio seed.
const RADIO_LIMIT: usize = 100;
/// Acoustic-neighbor candidate pool pulled before diversification (we over-fetch
/// so the per-artist cap still leaves a full station).
const NEIGHBOR_POOL: usize = 400;
/// Max tracks from any one artist in a diversified "sounds like" station, so it
/// isn't 20 tracks by the seed's artist (MMR-style cap).
const ARTIST_CAP: usize = 3;
/// Max tracks from any one album in a diversified station.
const ALBUM_CAP: usize = 2;
/// Default neighbor count for the "Sounds like this" shelf.
pub const SIMILAR_DEFAULT: usize = 20;
/// Default size of a playlist-recommendation pool (the client shows ~10 and
/// keeps the rest as a buffer to backfill as songs are added).
pub const PLAYLIST_REC_DEFAULT: usize = 30;
/// Cap on how many playlist seeds we actually run through the index — bounds the
/// per-request cost (each seed is a full index scan) for very large playlists.
/// The most-recently-passed seeds beyond this are sampled out.
const REC_MAX_SEEDS: usize = 100;
/// Neighbors pulled per seed before aggregation.
const REC_NEIGHBORS_PER_SEED: usize = 200;

/// One home shelf: a titled list of albums.
#[derive(Debug, Clone)]
pub struct DiscoverSection {
    pub id: String,
    pub title: String,
    pub albums: Vec<Album>,
}

#[derive(Clone)]
pub struct RecommendationService {
    pub play_history: Arc<dyn PlayHistoryRepo>,
    pub favorites: Arc<dyn FavoriteRepo>,
    pub tracks: Arc<dyn TrackRepo>,
    pub albums: Arc<dyn AlbumRepo>,
    pub artists: Arc<dyn ArtistRepo>,
    /// Acoustic-similarity index (Phase 12). `None` when fingerprinting is
    /// disabled — `seed_track_id` radio + `similar_tracks` then fall back to
    /// behavioral (same-artist) results, so the feature never hard-fails.
    pub index: Option<Arc<dyn SimilarityIndex>>,
}

impl RecommendationService {
    pub fn new(
        play_history: Arc<dyn PlayHistoryRepo>,
        favorites: Arc<dyn FavoriteRepo>,
        tracks: Arc<dyn TrackRepo>,
        albums: Arc<dyn AlbumRepo>,
        artists: Arc<dyn ArtistRepo>,
    ) -> Self {
        Self {
            play_history,
            favorites,
            tracks,
            albums,
            artists,
            index: None,
        }
    }

    /// Attach the acoustic-similarity index (Phase 12), enabling true "sounds
    /// like" radio + the similar-tracks shelf.
    pub fn with_similarity(mut self, index: Arc<dyn SimilarityIndex>) -> Self {
        self.index = Some(index);
        self
    }

    /// Personalized home shelves (only the non-empty ones). Any authed user;
    /// `SECRET_KEY` rejected (no user to personalize for).
    pub async fn get_home(&self, caller: &Identity) -> Result<Vec<DiscoverSection>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;

        let mut sections = Vec::new();

        // 1. More from artists you love (top-played + favorited artists).
        let from_artists = self.albums_from_loved_artists(user_id).await?;
        if !from_artists.is_empty() {
            sections.push(DiscoverSection {
                id: "from_artists".into(),
                title: "More from artists you love".into(),
                albums: from_artists,
            });
        }

        // 2. Jump back in (albums from your recent plays).
        let jump_back = self.albums_from_recent_plays(user_id).await?;
        if !jump_back.is_empty() {
            sections.push(DiscoverSection {
                id: "jump_back_in".into(),
                title: "Jump back in".into(),
                albums: jump_back,
            });
        }

        // 3. Your favorite albums.
        let fav_albums = self.favorite_albums(user_id).await?;
        if !fav_albums.is_empty() {
            sections.push(DiscoverSection {
                id: "your_albums".into(),
                title: "Your favorite albums".into(),
                albums: fav_albums,
            });
        }

        // 4. Recently added (always available — the fresh-account fallback).
        let recent = self.albums.recent(SECTION_LIMIT as i64).await?;
        if !recent.is_empty() {
            sections.push(DiscoverSection {
                id: "recently_added".into(),
                title: "Recently added".into(),
                albums: recent,
            });
        }

        Ok(sections)
    }

    /// A radio queue seeded from a **track** (acoustic "sounds like"), an artist
    /// (its tracks), or an album (its tracks first, then the artist's other
    /// tracks). Pass exactly one seed; track takes precedence. Any authed
    /// identity.
    ///
    /// The track seed uses the acoustic-similarity index when the seed has an
    /// embedding; otherwise (analysis pending / fingerprinting off) it **falls
    /// back** to behavioral radio seeded by the track's artist — so it never
    /// hard-fails while the first analysis pass is still running.
    pub async fn get_radio(
        &self,
        caller: &Identity,
        seed_artist_id: Option<Uuid>,
        seed_album_id: Option<Uuid>,
        seed_track_id: Option<Uuid>,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;

        if let Some(track_id) = seed_track_id {
            return self.radio_from_track(track_id).await;
        }

        let mut out: Vec<Track> = Vec::new();
        if let Some(album_id) = seed_album_id {
            let album = self
                .albums
                .get(album_id)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("album {album_id}")))?;
            // The seed album first, then the rest of the artist's catalog.
            self.extend_album_tracks(&mut out, album_id).await?;
            for a in self.albums.list_by_artist(album.artist_id).await? {
                if a.id != album_id {
                    self.extend_album_tracks(&mut out, a.id).await?;
                }
            }
        } else if let Some(artist_id) = seed_artist_id {
            if self.artists.get(artist_id).await?.is_none() {
                return Err(AppError::NotFound(format!("artist {artist_id}")));
            }
            for a in self.albums.list_by_artist(artist_id).await? {
                self.extend_album_tracks(&mut out, a.id).await?;
            }
        } else {
            return Err(AppError::InvalidArgument(
                "a radio seed (artist or album) is required".into(),
            ));
        }

        out.truncate(RADIO_LIMIT);
        Ok(out)
    }

    /// "Sounds like this" — the seed track's acoustic neighbors (no diversity
    /// cap; this is the raw similarity list for a shelf). Falls back to the
    /// artist's other tracks when the seed has no embedding. Any authed user.
    pub async fn similar_tracks(
        &self,
        caller: &Identity,
        track_id: Uuid,
        limit: usize,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;
        let seed = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;
        let limit = limit.clamp(1, RADIO_LIMIT);

        if let Some(neighbors) = self.acoustic_neighbors(track_id, limit).await? {
            return Ok(neighbors);
        }
        // Behavioral fallback: other tracks by the same artist.
        self.artist_tracks_excluding(seed.artist_id, track_id, limit).await
    }

    /// Spotify-style **playlist recommendations**: aggregate the acoustic
    /// neighbors of every seed track (the playlist's current songs), exclude the
    /// seeds, and return a diversified ranked pool. A candidate similar to *many*
    /// playlist songs scores higher (similarities are summed across seeds), so
    /// the more analyzed songs the playlist has, the stronger the signal.
    ///
    /// The client passes the playlist's **current** track ids each call, so a
    /// refresh after adding songs naturally re-bases the recommendations on the
    /// updated playlist. Falls back to same-artist suggestions (weighted by how
    /// often each artist appears in the playlist) when no seed has an embedding
    /// yet — so it always returns something useful. Any authed user.
    pub async fn recommend_for_playlist(
        &self,
        caller: &Identity,
        seed_track_ids: &[Uuid],
        limit: usize,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;
        let limit = limit.clamp(1, RADIO_LIMIT);
        if seed_track_ids.is_empty() {
            return Ok(Vec::new());
        }
        let exclude: HashSet<Uuid> = seed_track_ids.iter().copied().collect();

        // Acoustic path: sum neighbor similarities across the (analyzed) seeds.
        if let Some(index) = &self.index {
            let mut scores: std::collections::HashMap<Uuid, f32> = Default::default();
            for &seed in seed_track_ids.iter().take(REC_MAX_SEEDS) {
                if !index.has(seed).await {
                    continue;
                }
                for (id, sim) in index.nearest(seed, REC_NEIGHBORS_PER_SEED).await? {
                    if !exclude.contains(&id) {
                        *scores.entry(id).or_insert(0.0) += sim;
                    }
                }
            }
            if !scores.is_empty() {
                let mut ranked: Vec<(Uuid, f32)> = scores.into_iter().collect();
                ranked.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                });
                let mut candidates = Vec::with_capacity(ranked.len());
                for (id, _) in ranked {
                    if let Some(t) = self.tracks.get(id).await? {
                        candidates.push(t);
                    }
                }
                let mut out = Vec::new();
                diversify_into(&mut out, candidates, limit);
                return Ok(out);
            }
        }

        // Behavioral fallback: the playlist's artists, weighted by frequency.
        self.recommend_behavioral(seed_track_ids, &exclude, limit).await
    }

    /// Behavioral playlist recommendations: pull tracks from the playlist's
    /// artists (most-represented first), excluding what's already in it.
    async fn recommend_behavioral(
        &self,
        seed_track_ids: &[Uuid],
        exclude: &HashSet<Uuid>,
        limit: usize,
    ) -> Result<Vec<Track>> {
        let mut artist_counts: std::collections::HashMap<Uuid, usize> = Default::default();
        let mut order: Vec<Uuid> = Vec::new();
        for &seed in seed_track_ids.iter().take(REC_MAX_SEEDS) {
            if let Some(t) = self.tracks.get(seed).await? {
                let c = artist_counts.entry(t.artist_id).or_insert(0);
                if *c == 0 {
                    order.push(t.artist_id);
                }
                *c += 1;
            }
        }
        // Most-represented artists first (stable on first-seen order otherwise).
        order.sort_by(|a, b| artist_counts[b].cmp(&artist_counts[a]));

        let mut candidates: Vec<Track> = Vec::new();
        let mut seen: HashSet<Uuid> = exclude.clone();
        for artist_id in order {
            for al in self.albums.list_by_artist(artist_id).await? {
                for t in self.tracks.list_by_album(al.id).await? {
                    if seen.insert(t.id) {
                        candidates.push(t);
                    }
                }
            }
        }
        let mut out = Vec::new();
        diversify_into(&mut out, candidates, limit);
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Acoustic "sounds like" (Phase 12)
    // -----------------------------------------------------------------------

    /// Build a diversified radio station from a seed track's acoustic neighbors,
    /// or fall back to artist radio when the seed has no embedding yet.
    async fn radio_from_track(&self, track_id: Uuid) -> Result<Vec<Track>> {
        let seed = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;

        if let Some(neighbors) = self.acoustic_neighbors(track_id, NEIGHBOR_POOL).await? {
            // Seed track first, then diversified neighbors (per-artist/album cap).
            let mut out = vec![seed];
            diversify_into(&mut out, neighbors, RADIO_LIMIT);
            return Ok(out);
        }

        // Fallback: behavioral artist radio (the seed's artist catalog), seed first.
        let mut out = vec![seed.clone()];
        self.append_artist_radio(&mut out, seed.artist_id, seed.id).await?;
        out.truncate(RADIO_LIMIT);
        Ok(out)
    }

    /// The seed's nearest acoustic neighbors hydrated to `Track`s (nearest
    /// first, seed excluded), or `None` when there's no usable embedding (index
    /// disabled, or seed not yet analyzed) — signaling the caller to fall back.
    async fn acoustic_neighbors(
        &self,
        seed: Uuid,
        k: usize,
    ) -> Result<Option<Vec<Track>>> {
        let Some(index) = &self.index else {
            return Ok(None);
        };
        if !index.has(seed).await {
            return Ok(None);
        }
        let ranked = index.nearest(seed, k).await?;
        let mut out = Vec::with_capacity(ranked.len());
        for (id, _score) in ranked {
            if let Some(t) = self.tracks.get(id).await? {
                out.push(t);
            }
        }
        Ok(Some(out))
    }

    /// Append an artist's catalog (album by album) to `out`, skipping the seed
    /// track, up to `RADIO_LIMIT`.
    async fn append_artist_radio(
        &self,
        out: &mut Vec<Track>,
        artist_id: Uuid,
        skip_track: Uuid,
    ) -> Result<()> {
        for a in self.albums.list_by_artist(artist_id).await? {
            for t in self.tracks.list_by_album(a.id).await? {
                if t.id == skip_track {
                    continue;
                }
                out.push(t);
                if out.len() >= RADIO_LIMIT {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Other tracks by `artist_id`, excluding `skip_track`, up to `limit`.
    async fn artist_tracks_excluding(
        &self,
        artist_id: Uuid,
        skip_track: Uuid,
        limit: usize,
    ) -> Result<Vec<Track>> {
        let mut out = Vec::new();
        for a in self.albums.list_by_artist(artist_id).await? {
            for t in self.tracks.list_by_album(a.id).await? {
                if t.id == skip_track {
                    continue;
                }
                out.push(t);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // Shelf builders
    // -----------------------------------------------------------------------

    async fn albums_from_loved_artists(&self, user_id: Uuid) -> Result<Vec<Album>> {
        // Top-played artists first, then favorited artists not already covered.
        let mut artist_ids: Vec<Uuid> = self
            .play_history
            .top_artists(user_id, OffsetDateTime::UNIX_EPOCH, ARTIST_CANDIDATES)
            .await?
            .into_iter()
            .filter_map(|s| s.artist_id)
            .collect();
        let mut seen_artists: HashSet<Uuid> = artist_ids.iter().copied().collect();
        for id in self.favorites.list_ids(user_id, FavoriteKind::Artist).await? {
            if seen_artists.insert(id) {
                artist_ids.push(id);
            }
        }

        let mut albums = Vec::new();
        let mut seen_albums = HashSet::new();
        for artist_id in artist_ids {
            for album in self.albums.list_by_artist(artist_id).await? {
                if seen_albums.insert(album.id) {
                    albums.push(album);
                    if albums.len() >= SECTION_LIMIT {
                        return Ok(albums);
                    }
                }
            }
        }
        Ok(albums)
    }

    async fn albums_from_recent_plays(&self, user_id: Uuid) -> Result<Vec<Album>> {
        let plays = self.play_history.recent(user_id, RECENT_PLAYS_SCAN, 0).await?;
        let mut albums = Vec::new();
        let mut seen = HashSet::new();
        for p in plays {
            let Some(album_id) = p.album_id else { continue };
            if !seen.insert(album_id) {
                continue;
            }
            if let Some(a) = self.albums.get(album_id).await? {
                albums.push(a);
                if albums.len() >= SECTION_LIMIT {
                    break;
                }
            }
        }
        Ok(albums)
    }

    async fn favorite_albums(&self, user_id: Uuid) -> Result<Vec<Album>> {
        let ids = self.favorites.list_ids(user_id, FavoriteKind::Album).await?;
        let mut albums = Vec::new();
        for id in ids.into_iter().take(SECTION_LIMIT) {
            if let Some(a) = self.albums.get(id).await? {
                albums.push(a);
            }
        }
        Ok(albums)
    }

    async fn extend_album_tracks(&self, out: &mut Vec<Track>, album_id: Uuid) -> Result<()> {
        if out.len() >= RADIO_LIMIT {
            return Ok(());
        }
        for t in self.tracks.list_by_album(album_id).await? {
            out.push(t);
            if out.len() >= RADIO_LIMIT {
                break;
            }
        }
        Ok(())
    }

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity has no user to personalize discover; log in as a user".into(),
            )
        })
    }
}

/// Greedily append similarity-ordered `candidates` onto `out`, skipping any that
/// would push an artist past [`ARTIST_CAP`] or an album past [`ALBUM_CAP`], until
/// `out` reaches `limit`. This is the MMR-style diversification that keeps a
/// "sounds like" station from collapsing onto one artist. Tracks already in
/// `out` (e.g. the seed) seed the caps so they count against them.
fn diversify_into(out: &mut Vec<Track>, candidates: Vec<Track>, limit: usize) {
    let mut per_artist: HashSet<Uuid> = HashSet::new();
    let mut artist_counts: std::collections::HashMap<Uuid, usize> = Default::default();
    let mut album_counts: std::collections::HashMap<Uuid, usize> = Default::default();
    let mut seen: HashSet<Uuid> = HashSet::new();
    // Seed the caps + dedup set from whatever's already in `out`.
    for t in out.iter() {
        *artist_counts.entry(t.artist_id).or_default() += 1;
        *album_counts.entry(t.album_id).or_default() += 1;
        seen.insert(t.id);
        per_artist.insert(t.artist_id);
    }

    // First pass: respect the per-artist/album caps.
    let mut overflow: Vec<Track> = Vec::new();
    for t in candidates {
        if out.len() >= limit {
            return;
        }
        if seen.contains(&t.id) {
            continue;
        }
        let ac = artist_counts.entry(t.artist_id).or_default();
        let alc = album_counts.get(&t.album_id).copied().unwrap_or(0);
        if *ac >= ARTIST_CAP || alc >= ALBUM_CAP {
            overflow.push(t);
            continue;
        }
        *ac += 1;
        *album_counts.entry(t.album_id).or_default() += 1;
        seen.insert(t.id);
        out.push(t);
    }
    // Second pass: if the caps left us short of a full station, backfill from the
    // overflow (still similarity-ordered) so we don't return a stub queue.
    for t in overflow {
        if out.len() >= limit {
            return;
        }
        if seen.insert(t.id) {
            out.push(t);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests against in-memory fakes (no live Postgres).

    use super::*;
    use crate::db::models::{
        Artist, NewAlbum, NewArtist, NewPlayEvent, NewTrack, PermissionLevel, PlayEvent,
    };
    use crate::db::repo::{ArtistPlayStat, PlayTotals, TrackIdPath, TrackPlayStat};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // --- Play-history fake: only top_artists + recent matter here. ---
    #[derive(Default)]
    struct FakePlays {
        recent: Mutex<Vec<PlayEvent>>,
        top_artists: Mutex<Vec<ArtistPlayStat>>,
    }
    #[async_trait]
    impl PlayHistoryRepo for FakePlays {
        async fn record_many(&self, _: &[NewPlayEvent]) -> Result<u64> {
            Ok(0)
        }
        async fn recent(&self, _: Uuid, _: i64, _: i64) -> Result<Vec<PlayEvent>> {
            Ok(self.recent.lock().unwrap().clone())
        }
        async fn top_tracks(&self, _: Uuid, _: OffsetDateTime, _: i64) -> Result<Vec<TrackPlayStat>> {
            Ok(vec![])
        }
        async fn top_artists(
            &self,
            _: Uuid,
            _: OffsetDateTime,
            _: i64,
        ) -> Result<Vec<ArtistPlayStat>> {
            Ok(self.top_artists.lock().unwrap().clone())
        }
        async fn totals(&self, _: Uuid, _: OffsetDateTime) -> Result<PlayTotals> {
            Ok(PlayTotals::default())
        }
        async fn play_count(&self, _: Uuid, _: Uuid) -> Result<i64> {
            Ok(0)
        }
    }

    // --- Favorites fake ---
    #[derive(Default)]
    struct FakeFavorites {
        rows: Mutex<Vec<(Uuid, &'static str, Uuid)>>,
    }
    #[async_trait]
    impl FavoriteRepo for FakeFavorites {
        async fn add(&self, _: Uuid, _: FavoriteKind, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn remove(&self, _: Uuid, _: FavoriteKind, _: Uuid) -> Result<()> {
            Ok(())
        }
        async fn is_favorite(&self, _: Uuid, _: FavoriteKind, _: Uuid) -> Result<bool> {
            Ok(false)
        }
        async fn list_ids(&self, user_id: Uuid, kind: FavoriteKind) -> Result<Vec<Uuid>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(u, k, _)| *u == user_id && *k == kind.as_str())
                .map(|(_, _, e)| *e)
                .collect())
        }
    }

    // --- Albums fake ---
    #[derive(Default)]
    struct FakeAlbums {
        rows: Mutex<Vec<Album>>,
    }
    impl FakeAlbums {
        fn insert(&self, artist_id: Uuid, title: &str) -> Album {
            let a = Album {
                id: Uuid::new_v4(),
                artist_id,
                title: title.into(),
                release_year: None,
                album_type: "album".into(),
                cover_path: None,
                storage_bytes: 0,
                created_at: now(),
                updated_at: now(),
            };
            self.rows.lock().unwrap().push(a.clone());
            a
        }
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
        async fn recent(&self, limit: i64) -> Result<Vec<Album>> {
            let mut v = self.rows.lock().unwrap().clone();
            v.reverse();
            v.truncate(limit.max(0) as usize);
            Ok(v)
        }
        async fn search(&self, _: &str, _: i64, _: i64) -> Result<Vec<Album>> {
            Ok(vec![])
        }
        async fn update(&self, _: Uuid, _: &str, _: Option<i32>, _: Option<&str>) -> Result<Option<Album>> {
            Ok(None)
        }
        async fn set_album_type(&self, _: Uuid, _: &str) -> Result<Option<Album>> {
            Ok(None)
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

    // --- Tracks fake ---
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
                title: title.into(),
                track_no: None,
                disc_no: None,
                duration_ms: 1000,
                codec: "flac".into(),
                bitrate_kbps: None,
                file_path: format!("/{title}.flac"),
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
        async fn update(&self, _: Uuid, _: &str, _: Option<i32>, _: Option<i32>, _: &str) -> Result<Option<Track>> {
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

    // --- Artists fake ---
    #[derive(Default)]
    struct FakeArtists {
        rows: Mutex<Vec<Artist>>,
    }
    impl FakeArtists {
        fn insert(&self, name: &str) -> Artist {
            let a = Artist {
                id: Uuid::new_v4(),
                name: name.into(),
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
        async fn create(&self, _: NewArtist) -> Result<Artist> {
            unreachable!()
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

    struct Fakes {
        svc: RecommendationService,
        plays: Arc<FakePlays>,
        favs: Arc<FakeFavorites>,
        tracks: Arc<FakeTracks>,
        albums: Arc<FakeAlbums>,
        artists: Arc<FakeArtists>,
    }

    fn make() -> Fakes {
        let plays = Arc::new(FakePlays::default());
        let favs = Arc::new(FakeFavorites::default());
        let tracks = Arc::new(FakeTracks::default());
        let albums = Arc::new(FakeAlbums::default());
        let artists = Arc::new(FakeArtists::default());
        let svc = RecommendationService::new(
            plays.clone(),
            favs.clone(),
            tracks.clone(),
            albums.clone(),
            artists.clone(),
        );
        Fakes { svc, plays, favs, tracks, albums, artists }
    }

    fn user() -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level: PermissionLevel::User,
        }
    }

    #[tokio::test]
    async fn home_recently_added_only_for_fresh_user() {
        let f = make();
        let artist = f.artists.insert("A");
        f.albums.insert(artist.id, "Newest");
        // No plays, no favorites → only "Recently added".
        let sections = f.svc.get_home(&user()).await.unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].id, "recently_added");
        assert_eq!(sections[0].albums.len(), 1);
    }

    #[tokio::test]
    async fn home_includes_loved_artist_and_favorite_album_shelves() {
        let f = make();
        let me = user();
        let uid = me.user_id().unwrap();
        let top = f.artists.insert("Top");
        let top_album = f.albums.insert(top.id, "Top Album");
        let fav_artist_album = f.albums.insert(top.id, "Another");

        // Top artist from play history.
        f.plays.top_artists.lock().unwrap().push(ArtistPlayStat {
            artist_id: Some(top.id),
            artist_name: "Top".into(),
            plays: 5,
        });
        // A favorited album.
        f.favs
            .rows
            .lock()
            .unwrap()
            .push((uid, "album", fav_artist_album.id));

        let sections = f.svc.get_home(&me).await.unwrap();
        let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"from_artists"));
        assert!(ids.contains(&"your_albums"));
        assert!(ids.contains(&"recently_added"));
        // The "from artists" shelf has the top artist's albums.
        let from = sections.iter().find(|s| s.id == "from_artists").unwrap();
        assert!(from.albums.iter().any(|a| a.id == top_album.id));
    }

    #[tokio::test]
    async fn secret_key_cannot_get_home() {
        let f = make();
        let err = f.svc.get_home(&Identity::SecretKey).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn radio_from_artist_returns_their_tracks() {
        let f = make();
        let artist = f.artists.insert("R");
        let al1 = f.albums.insert(artist.id, "One");
        let al2 = f.albums.insert(artist.id, "Two");
        f.tracks.insert(artist.id, al1.id, "t1");
        f.tracks.insert(artist.id, al2.id, "t2");

        let tracks = f.svc.get_radio(&user(), Some(artist.id), None, None).await.unwrap();
        assert_eq!(tracks.len(), 2);
    }

    #[tokio::test]
    async fn radio_from_album_puts_seed_album_first() {
        let f = make();
        let artist = f.artists.insert("R");
        let seed = f.albums.insert(artist.id, "Seed");
        let other = f.albums.insert(artist.id, "Other");
        let seed_track = f.tracks.insert(artist.id, seed.id, "seedtrack");
        f.tracks.insert(artist.id, other.id, "othertrack");

        let tracks = f.svc.get_radio(&user(), None, Some(seed.id), None).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, seed_track.id, "seed album's track comes first");
    }

    #[tokio::test]
    async fn radio_requires_a_seed() {
        let f = make();
        let err = f.svc.get_radio(&user(), None, None, None).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    // --- Acoustic "sounds like" (Phase 12) ---

    use crate::services::fingerprint::SimilarityIndex;

    /// A fake index returning a fixed neighbor ranking for one seed.
    #[derive(Default)]
    struct FakeIndex {
        // seed -> ordered (neighbor_id, score)
        ranks: Mutex<std::collections::HashMap<Uuid, Vec<(Uuid, f32)>>>,
    }
    #[async_trait]
    impl SimilarityIndex for FakeIndex {
        async fn nearest(&self, seed: Uuid, k: usize) -> Result<Vec<(Uuid, f32)>> {
            Ok(self
                .ranks
                .lock()
                .unwrap()
                .get(&seed)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(k)
                .collect())
        }
        async fn has(&self, seed: Uuid) -> bool {
            self.ranks.lock().unwrap().contains_key(&seed)
        }
        async fn reload(&self) -> Result<()> {
            Ok(())
        }
        async fn len(&self) -> usize {
            self.ranks.lock().unwrap().len()
        }
    }

    #[tokio::test]
    async fn radio_from_track_uses_acoustic_neighbors_and_caps_artists() {
        let f = make();
        let idx = Arc::new(FakeIndex::default());
        let svc = f.svc.clone().with_similarity(idx.clone());

        // Seed track by artist A.
        let artist_a = f.artists.insert("A");
        let alb_a = f.albums.insert(artist_a.id, "AlbA");
        let seed = f.tracks.insert(artist_a.id, alb_a.id, "seed");

        // Build 6 neighbors all by artist B across 1 album — the artist cap (3)
        // must limit how many land before backfill.
        let artist_b = f.artists.insert("B");
        let alb_b = f.albums.insert(artist_b.id, "AlbB");
        let mut ranks = Vec::new();
        for i in 0..6 {
            let t = f.tracks.insert(artist_b.id, alb_b.id, &format!("b{i}"));
            ranks.push((t.id, 1.0 - i as f32 * 0.1));
        }
        idx.ranks.lock().unwrap().insert(seed.id, ranks);

        let out = svc.get_radio(&user(), None, None, Some(seed.id)).await.unwrap();
        // Seed first.
        assert_eq!(out[0].id, seed.id);
        // All 6 neighbors share one album → album cap (2) bounds the *capped*
        // portion; the rest backfill, so the full station is still returned.
        assert_eq!(out.len(), 7);
    }

    #[tokio::test]
    async fn radio_from_track_falls_back_when_no_embedding() {
        let f = make();
        let idx = Arc::new(FakeIndex::default()); // empty → seed has no embedding
        let svc = f.svc.clone().with_similarity(idx);

        let artist = f.artists.insert("A");
        let alb = f.albums.insert(artist.id, "Alb");
        let seed = f.tracks.insert(artist.id, alb.id, "seed");
        let sibling = f.tracks.insert(artist.id, alb.id, "sibling");

        let out = svc.get_radio(&user(), None, None, Some(seed.id)).await.unwrap();
        // Behavioral fallback: seed first, then the artist's other tracks.
        assert_eq!(out[0].id, seed.id);
        assert!(out.iter().any(|t| t.id == sibling.id));
    }

    #[tokio::test]
    async fn radio_from_unknown_track_is_404() {
        let f = make();
        let svc = f.svc.clone().with_similarity(Arc::new(FakeIndex::default()));
        let err = svc
            .get_radio(&user(), None, None, Some(Uuid::new_v4()))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn similar_tracks_returns_neighbors_then_falls_back() {
        let f = make();
        let idx = Arc::new(FakeIndex::default());
        let svc = f.svc.clone().with_similarity(idx.clone());

        let artist = f.artists.insert("A");
        let alb = f.albums.insert(artist.id, "Alb");
        let seed = f.tracks.insert(artist.id, alb.id, "seed");
        let n1 = f.tracks.insert(f.artists.insert("B").id, f.albums.insert(Uuid::new_v4(), "x").id, "n1");
        idx.ranks.lock().unwrap().insert(seed.id, vec![(n1.id, 0.9)]);

        let out = svc.similar_tracks(&user(), seed.id, 5).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, n1.id);

        // A track with no embedding falls back to same-artist tracks.
        let other = f.tracks.insert(artist.id, alb.id, "other");
        let fb = svc.similar_tracks(&user(), other.id, 5).await.unwrap();
        assert!(fb.iter().any(|t| t.id == seed.id));
    }

    #[tokio::test]
    async fn playlist_recs_aggregate_neighbors_and_exclude_seeds() {
        let f = make();
        let idx = Arc::new(FakeIndex::default());
        let svc = f.svc.clone().with_similarity(idx.clone());

        // Two playlist seeds, each by a different artist.
        let ar1 = f.artists.insert("S1");
        let ar2 = f.artists.insert("S2");
        let al1 = f.albums.insert(ar1.id, "A1");
        let al2 = f.albums.insert(ar2.id, "A2");
        let seed1 = f.tracks.insert(ar1.id, al1.id, "seed1");
        let seed2 = f.tracks.insert(ar2.id, al2.id, "seed2");

        // A "shared" candidate is a neighbor of BOTH seeds (summed score wins);
        // a "single" candidate neighbors only one. Each from its own artist so
        // the per-artist diversity cap doesn't drop them.
        let arc = f.artists.insert("Cand");
        let alc = f.albums.insert(arc.id, "C");
        let shared = f.tracks.insert(f.artists.insert("Shared").id, f.albums.insert(Uuid::new_v4(), "sh").id, "shared");
        let single = f.tracks.insert(arc.id, alc.id, "single");

        idx.ranks.lock().unwrap().insert(seed1.id, vec![(shared.id, 0.6), (single.id, 0.5)]);
        idx.ranks.lock().unwrap().insert(seed2.id, vec![(shared.id, 0.6)]);

        let recs = svc
            .recommend_for_playlist(&user(), &[seed1.id, seed2.id], 10)
            .await
            .unwrap();

        // Seeds are excluded; the doubly-neighbored "shared" ranks first.
        assert!(!recs.iter().any(|t| t.id == seed1.id || t.id == seed2.id));
        assert_eq!(recs[0].id, shared.id, "summed-score candidate first");
        assert!(recs.iter().any(|t| t.id == single.id));
    }

    #[tokio::test]
    async fn playlist_recs_fall_back_to_playlist_artists() {
        let f = make();
        let idx = Arc::new(FakeIndex::default()); // empty → no embeddings
        let svc = f.svc.clone().with_similarity(idx);

        let artist = f.artists.insert("A");
        let alb = f.albums.insert(artist.id, "Alb");
        let seed = f.tracks.insert(artist.id, alb.id, "seed");
        let sibling = f.tracks.insert(artist.id, alb.id, "sibling");

        let recs = svc
            .recommend_for_playlist(&user(), &[seed.id], 10)
            .await
            .unwrap();
        // Behavioral fallback surfaces the artist's other track, never the seed.
        assert!(recs.iter().any(|t| t.id == sibling.id));
        assert!(!recs.iter().any(|t| t.id == seed.id));
    }

    #[tokio::test]
    async fn playlist_recs_empty_for_no_seeds() {
        let f = make();
        let svc = f.svc.clone().with_similarity(Arc::new(FakeIndex::default()));
        assert!(svc.recommend_for_playlist(&user(), &[], 10).await.unwrap().is_empty());
    }
}
