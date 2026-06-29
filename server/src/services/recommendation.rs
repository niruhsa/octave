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

/// Albums per home shelf.
const SECTION_LIMIT: usize = 12;
/// Top-artist / favorite candidates considered for the "from artists" shelf.
const ARTIST_CANDIDATES: i64 = 10;
/// Recent plays scanned to derive the "jump back in" shelf.
const RECENT_PLAYS_SCAN: i64 = 60;
/// Max tracks returned by a radio seed.
const RADIO_LIMIT: usize = 100;

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
        }
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

    /// A radio queue seeded from an artist (its tracks) or an album (its tracks
    /// first, then the artist's other tracks). Any authed identity.
    pub async fn get_radio(
        &self,
        caller: &Identity,
        seed_artist_id: Option<Uuid>,
        seed_album_id: Option<Uuid>,
    ) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;

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

        let tracks = f.svc.get_radio(&user(), Some(artist.id), None).await.unwrap();
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

        let tracks = f.svc.get_radio(&user(), None, Some(seed.id)).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, seed_track.id, "seed album's track comes first");
    }

    #[tokio::test]
    async fn radio_requires_a_seed() {
        let f = make();
        let err = f.svc.get_radio(&user(), None, None).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }
}
