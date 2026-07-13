//! Favorites (Phase 11).
//!
//! A per-user "like" on a track, album, or artist. Any authed *user* may
//! favorite (the `SECRET_KEY` identity has no `user_id`, so it is rejected —
//! there's no user to own the favorite). Add/remove are audited
//! (`favorite.add` / `favorite.remove`), mirroring follows.
//!
//! List reads resolve the favorited ids to full catalog entities so the client
//! can render + play them directly (favorite lists are modest, so per-id
//! resolution is fine — same approach as the follow list).

use std::sync::Arc;

use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{Album, Artist, FavoriteKind, NewAuditEntry, PermissionLevel, Track};
use crate::db::repo::{AlbumRepo, ArtistRepo, AuditRepo, FavoriteRepo, TrackRepo};
use crate::error::{AppError, Result};

#[derive(Clone)]
pub struct FavoritesService {
    pub favorites: Arc<dyn FavoriteRepo>,
    pub tracks: Arc<dyn TrackRepo>,
    pub albums: Arc<dyn AlbumRepo>,
    pub artists: Arc<dyn ArtistRepo>,
    pub audit: Arc<dyn AuditRepo>,
}

impl FavoritesService {
    pub fn new(
        favorites: Arc<dyn FavoriteRepo>,
        tracks: Arc<dyn TrackRepo>,
        albums: Arc<dyn AlbumRepo>,
        artists: Arc<dyn ArtistRepo>,
        audit: Arc<dyn AuditRepo>,
    ) -> Self {
        Self {
            favorites,
            tracks,
            albums,
            artists,
            audit,
        }
    }

    /// Favorite an entity. Idempotent. Any authed user; `SECRET_KEY` rejected.
    /// 404s if the entity doesn't exist. Audited.
    pub async fn favorite(
        &self,
        caller: &Identity,
        kind: FavoriteKind,
        entity_id: Uuid,
    ) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        if !self.entity_exists(kind, entity_id).await? {
            return Err(AppError::NotFound(format!("{} {entity_id}", kind.as_str())));
        }
        self.favorites.add(user_id, kind, entity_id).await?;
        self.audit(caller, "favorite.add", kind, entity_id).await?;
        Ok(())
    }

    /// Unfavorite an entity. Idempotent. Audited.
    pub async fn unfavorite(
        &self,
        caller: &Identity,
        kind: FavoriteKind,
        entity_id: Uuid,
    ) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.favorites.remove(user_id, kind, entity_id).await?;
        self.audit(caller, "favorite.remove", kind, entity_id)
            .await?;
        Ok(())
    }

    /// Whether the caller has favorited `entity_id`.
    pub async fn is_favorite(
        &self,
        caller: &Identity,
        kind: FavoriteKind,
        entity_id: Uuid,
    ) -> Result<bool> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.favorites.is_favorite(user_id, kind, entity_id).await
    }

    /// The caller's favorited tracks (full rows, newest first). A since-deleted
    /// track is dropped (the FK cascade removes the favorite anyway).
    pub async fn list_tracks(&self, caller: &Identity) -> Result<Vec<Track>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let ids = self
            .favorites
            .list_ids(user_id, FavoriteKind::Track)
            .await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.tracks.get(id).await? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// The caller's favorited albums (full rows, newest first).
    pub async fn list_albums(&self, caller: &Identity) -> Result<Vec<Album>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let ids = self
            .favorites
            .list_ids(user_id, FavoriteKind::Album)
            .await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(a) = self.albums.get(id).await? {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// The caller's favorited artists (full rows, newest first).
    pub async fn list_artists(&self, caller: &Identity) -> Result<Vec<Artist>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let ids = self
            .favorites
            .list_ids(user_id, FavoriteKind::Artist)
            .await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(a) = self.artists.get(id).await? {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// Just the favorited track ids — for bulk heart-state hydration in the
    /// client (the now-playing bar + track rows) without N round-trips.
    pub async fn favorited_track_ids(&self, caller: &Identity) -> Result<Vec<Uuid>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.favorites.list_ids(user_id, FavoriteKind::Track).await
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn entity_exists(&self, kind: FavoriteKind, id: Uuid) -> Result<bool> {
        Ok(match kind {
            FavoriteKind::Track => self.tracks.get(id).await?.is_some(),
            FavoriteKind::Album => self.albums.get(id).await?.is_some(),
            FavoriteKind::Artist => self.artists.get(id).await?.is_some(),
        })
    }

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity has no user to own favorites; log in as a user".into(),
            )
        })
    }

    async fn audit(
        &self,
        caller: &Identity,
        action: &str,
        kind: FavoriteKind,
        entity_id: Uuid,
    ) -> Result<()> {
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: action.to_string(),
                entity_type: kind.as_str().to_string(),
                entity_id: Some(entity_id),
                before_json: None,
                after_json: Some(
                    serde_json::json!({ "kind": kind.as_str(), "entity_id": entity_id })
                        .to_string(),
                ),
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests against in-memory fakes (no live Postgres). Validate the
    //! permission rules, entity-existence check, idempotent add, list
    //! resolution, and audit writes.

    use super::*;
    use crate::db::models::{AuditEntry, NewAlbum, NewArtist, NewTrack, PermissionLevel};
    use crate::db::repo::{AlbumRepo, ArtistRepo, AuditRepo, TrackIdPath};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // ---- Favorites fake ----
    #[derive(Default)]
    struct FakeFavorites {
        // (user_id, kind, entity_id)
        rows: Mutex<Vec<(Uuid, &'static str, Uuid)>>,
    }
    #[async_trait]
    impl FavoriteRepo for FakeFavorites {
        async fn add(&self, user_id: Uuid, kind: FavoriteKind, entity_id: Uuid) -> Result<()> {
            let mut g = self.rows.lock().unwrap();
            if !g
                .iter()
                .any(|(u, k, e)| *u == user_id && *k == kind.as_str() && *e == entity_id)
            {
                g.push((user_id, kind.as_str(), entity_id));
            }
            Ok(())
        }
        async fn remove(&self, user_id: Uuid, kind: FavoriteKind, entity_id: Uuid) -> Result<()> {
            self.rows
                .lock()
                .unwrap()
                .retain(|(u, k, e)| !(*u == user_id && *k == kind.as_str() && *e == entity_id));
            Ok(())
        }
        async fn is_favorite(
            &self,
            user_id: Uuid,
            kind: FavoriteKind,
            entity_id: Uuid,
        ) -> Result<bool> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .any(|(u, k, e)| *u == user_id && *k == kind.as_str() && *e == entity_id))
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

    // ---- Tracks fake (get/insert) ----
    #[derive(Default)]
    struct FakeTracks {
        rows: Mutex<Vec<Track>>,
    }
    impl FakeTracks {
        fn insert(&self) -> Track {
            let t = Track {
                id: Uuid::new_v4(),
                album_id: Uuid::new_v4(),
                artist_id: Uuid::new_v4(),
                title: "T".into(),
                track_no: None,
                disc_no: None,
                duration_ms: 1000,
                codec: "flac".into(),
                bitrate_kbps: None,
                file_path: "/x.flac".into(),
                file_size: None,
                sample_rate_hz: None,
                bit_depth: None,
                channels: None,
                metadata_json: "{}".into(),
                is_single_release: false,
                is_explicit: false,
                lyrics_path: None,
                lyrics_synced: false,
                lyrics_source: None,
                lyrics_instrumental: false,
                lyrics_source_sig: None,
                lyrics_synced_at: None,
                loudness_lufs: None,
                loudness_peak: None,
                album_loudness_lufs: None,
                loudness_source_sig: None,
                loudness_analyzed_at: None,
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
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == id)
                .cloned())
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

    // ---- Albums fake (get only) ----
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
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.id == id)
                .cloned())
        }
        async fn list_by_artist(&self, _: Uuid) -> Result<Vec<Album>> {
            Ok(vec![])
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

    // ---- Artists fake (get/insert) ----
    #[derive(Default)]
    struct FakeArtists {
        rows: Mutex<Vec<Artist>>,
    }
    impl FakeArtists {
        fn insert(&self) -> Artist {
            let a = Artist {
                id: Uuid::new_v4(),
                name: "A".into(),
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
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.id == id)
                .cloned())
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

    // ---- Audit fake ----
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

    fn make() -> (
        FavoritesService,
        Arc<FakeTracks>,
        Arc<FakeAlbums>,
        Arc<FakeArtists>,
        Arc<FakeAudit>,
    ) {
        let favs = Arc::new(FakeFavorites::default());
        let tracks = Arc::new(FakeTracks::default());
        let albums = Arc::new(FakeAlbums::default());
        let artists = Arc::new(FakeArtists::default());
        let audit = Arc::new(FakeAudit::default());
        let svc = FavoritesService::new(
            favs,
            tracks.clone(),
            albums.clone(),
            artists.clone(),
            audit.clone(),
        );
        (svc, tracks, albums, artists, audit)
    }

    fn user() -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level: PermissionLevel::User,
        }
    }

    #[tokio::test]
    async fn favorite_track_then_list_and_audit() {
        let (svc, tracks, _al, _ar, audit) = make();
        let me = user();
        let t = tracks.insert();

        svc.favorite(&me, FavoriteKind::Track, t.id).await.unwrap();
        // Idempotent.
        svc.favorite(&me, FavoriteKind::Track, t.id).await.unwrap();

        assert!(
            svc.is_favorite(&me, FavoriteKind::Track, t.id)
                .await
                .unwrap()
        );
        let listed = svc.list_tracks(&me).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, t.id);
        assert_eq!(svc.favorited_track_ids(&me).await.unwrap(), vec![t.id]);

        svc.unfavorite(&me, FavoriteKind::Track, t.id)
            .await
            .unwrap();
        assert!(
            !svc.is_favorite(&me, FavoriteKind::Track, t.id)
                .await
                .unwrap()
        );
        assert!(svc.list_tracks(&me).await.unwrap().is_empty());

        let actions = audit.actions.lock().unwrap().clone();
        assert_eq!(
            actions,
            vec!["favorite.add", "favorite.add", "favorite.remove"]
        );
    }

    #[tokio::test]
    async fn favorite_unknown_entity_is_404() {
        let (svc, ..) = make();
        let me = user();
        let err = svc
            .favorite(&me, FavoriteKind::Album, Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn secret_key_rejected() {
        let (svc, tracks, ..) = make();
        let t = tracks.insert();
        let err = svc
            .favorite(&Identity::SecretKey, FavoriteKind::Track, t.id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        let err = svc.list_tracks(&Identity::SecretKey).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn favorites_are_per_kind_and_per_user() {
        let (svc, _t, _al, artists, _au) = make();
        let me = user();
        let other = user();
        let artist = artists.insert();

        svc.favorite(&me, FavoriteKind::Artist, artist.id)
            .await
            .unwrap();
        assert!(
            svc.is_favorite(&me, FavoriteKind::Artist, artist.id)
                .await
                .unwrap()
        );
        // A different kind for the same id is independent.
        assert!(
            !svc.is_favorite(&me, FavoriteKind::Track, artist.id)
                .await
                .unwrap()
        );
        // Another user doesn't see it.
        assert!(
            !svc.is_favorite(&other, FavoriteKind::Artist, artist.id)
                .await
                .unwrap()
        );
        assert_eq!(svc.list_artists(&me).await.unwrap().len(), 1);
        assert!(svc.list_artists(&other).await.unwrap().is_empty());
    }
}
