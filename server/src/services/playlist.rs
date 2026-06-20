//! Playlist management.
//!
//! Service-layer rules (defense in depth — transport already gated):
//! - Reads (`get_playlist`, `list_tracks`): any authed identity.
//! - Listing playlists owned by another user (`list_for_owner`): owner or
//!   `Manager+`.
//! - Mutations (rename, delete, add/remove/reorder tracks): the playlist's
//!   owner or `Manager+`.
//! - Creating a playlist binds it to a real user; the `SECRET_KEY` identity
//!   has no `user_id` and cannot own playlists, so it is rejected for create
//!   with `InvalidArgument`. Manager+ humans create their own playlists like
//!   anyone else.
//! - Every mutation writes a `playlist.*` audit_log row.
//! - Track positions are 1-based contiguous integers; reorder/insert shifts
//!   the underlying rows so positions stay contiguous after every op (see
//!   [`PlaylistRepo::insert_track_at`] etc.).

use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    self as m, NewAuditEntry, NewPlaylist, PermissionLevel, Playlist, PlaylistTrack,
};
use crate::db::repo::{AuditRepo, PlaylistRepo, TrackRepo, UserRepo};
use crate::error::{AppError, Result};

#[derive(Clone)]
pub struct PlaylistService {
    pub playlists: Arc<dyn PlaylistRepo>,
    pub tracks: Arc<dyn TrackRepo>,
    pub users: Arc<dyn UserRepo>,
    pub audit: Arc<dyn AuditRepo>,
}

/// Convenience view returned by reads: playlist + ordered tracks.
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistWithTracks {
    pub playlist: Playlist,
    pub tracks: Vec<PlaylistTrack>,
}

impl PlaylistService {
    pub fn new(
        playlists: Arc<dyn PlaylistRepo>,
        tracks: Arc<dyn TrackRepo>,
        users: Arc<dyn UserRepo>,
        audit: Arc<dyn AuditRepo>,
    ) -> Self {
        Self {
            playlists,
            tracks,
            users,
            audit,
        }
    }

    // -----------------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------------

    /// Create a playlist owned by the caller. Requires a real user identity
    /// (`SECRET_KEY` is rejected — it has no user to own the row).
    pub async fn create(&self, caller: &Identity, name: &str) -> Result<Playlist> {
        caller.require(PermissionLevel::User)?;
        let owner_id = self.caller_user_id(caller)?;
        let name = validate_name(name)?;

        let playlist = self
            .playlists
            .create(NewPlaylist {
                owner_id,
                name: name.to_string(),
            })
            .await?;
        self.audit(
            caller,
            "playlist.create",
            Some(playlist.id),
            None::<&Playlist>,
            Some(&playlist),
        )
        .await?;
        Ok(playlist)
    }

    /// Any authed identity may read a playlist (no per-row visibility column
    /// yet; treat playlists as public reads).
    pub async fn get(&self, caller: &Identity, id: Uuid) -> Result<Playlist> {
        caller.require(PermissionLevel::User)?;
        self.playlists
            .get(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("playlist {id}")))
    }

    pub async fn get_with_tracks(
        &self,
        caller: &Identity,
        id: Uuid,
    ) -> Result<PlaylistWithTracks> {
        let playlist = self.get(caller, id).await?;
        let tracks = self.playlists.list_tracks(id).await?;
        Ok(PlaylistWithTracks { playlist, tracks })
    }

    pub async fn list_for_owner(
        &self,
        caller: &Identity,
        owner_id: Uuid,
    ) -> Result<Vec<Playlist>> {
        caller.require(PermissionLevel::User)?;
        // Listing another user's playlists is a Manager+ operation; the
        // owner can always see their own.
        if !self.caller_owns_or_manages(caller, owner_id) {
            return Err(AppError::PermissionDenied(
                "cannot list another user's playlists without Manager".into(),
            ));
        }
        self.playlists.list_for_owner(owner_id).await
    }

    /// List the calling user's own playlists.
    pub async fn list_mine(&self, caller: &Identity) -> Result<Vec<Playlist>> {
        let owner_id = self.caller_user_id(caller)?;
        self.playlists.list_for_owner(owner_id).await
    }

    pub async fn rename(
        &self,
        caller: &Identity,
        id: Uuid,
        name: &str,
    ) -> Result<Playlist> {
        let before = self.require_owner_or_manager(caller, id).await?;
        let name = validate_name(name)?;
        let after = self
            .playlists
            .update_name(id, name)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("playlist {id}")))?;
        self.audit(
            caller,
            "playlist.update",
            Some(id),
            Some(&before),
            Some(&after),
        )
        .await?;
        Ok(after)
    }

    pub async fn delete(&self, caller: &Identity, id: Uuid) -> Result<bool> {
        let before = match self.playlists.get(id).await? {
            Some(p) => p,
            None => return Ok(false),
        };
        self.require_owner_or_manager_on(caller, &before)?;
        self.playlists.delete(id).await?;
        self.audit(
            caller,
            "playlist.delete",
            Some(id),
            Some(&before),
            None::<&Playlist>,
        )
        .await?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Track ops
    // -----------------------------------------------------------------------

    /// Append a track to the end of the playlist.
    pub async fn add_track(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
        track_id: Uuid,
    ) -> Result<PlaylistTrack> {
        self.require_owner_or_manager(caller, playlist_id).await?;
        // Validate FK with a clearer error than DB constraint failure.
        if self.tracks.get(track_id).await?.is_none() {
            return Err(AppError::NotFound(format!("track {track_id}")));
        }
        let position = self.playlists.next_position(playlist_id).await?;
        let row = self
            .playlists
            .insert_track_at(playlist_id, track_id, position)
            .await?;
        self.audit(
            caller,
            "playlist.track.add",
            Some(playlist_id),
            None::<&PlaylistTrack>,
            Some(&row),
        )
        .await?;
        Ok(row)
    }

    /// Insert a track at a specific 1-based position, shifting later rows up.
    pub async fn insert_track(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
        track_id: Uuid,
        position: i32,
    ) -> Result<PlaylistTrack> {
        self.require_owner_or_manager(caller, playlist_id).await?;
        if self.tracks.get(track_id).await?.is_none() {
            return Err(AppError::NotFound(format!("track {track_id}")));
        }
        let next = self.playlists.next_position(playlist_id).await?;
        let clamped = position.max(1).min(next);
        let row = self
            .playlists
            .insert_track_at(playlist_id, track_id, clamped)
            .await?;
        self.audit(
            caller,
            "playlist.track.add",
            Some(playlist_id),
            None::<&PlaylistTrack>,
            Some(&row),
        )
        .await?;
        Ok(row)
    }

    /// Remove the row at `position`. Returns the removed `PlaylistTrack` or
    /// `None` if the position was empty.
    pub async fn remove_track_at(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
        position: i32,
    ) -> Result<Option<PlaylistTrack>> {
        self.require_owner_or_manager(caller, playlist_id).await?;
        let before = self
            .playlists
            .get_track_at(playlist_id, position)
            .await?;
        let Some(before) = before else {
            return Ok(None);
        };
        let removed = self
            .playlists
            .remove_track_at(playlist_id, position)
            .await?;
        if !removed {
            return Ok(None);
        }
        self.audit(
            caller,
            "playlist.track.remove",
            Some(playlist_id),
            Some(&before),
            None::<&PlaylistTrack>,
        )
        .await?;
        Ok(Some(before))
    }

    /// Move a track from one position to another within the playlist.
    pub async fn reorder(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
        from: i32,
        to: i32,
    ) -> Result<()> {
        self.require_owner_or_manager(caller, playlist_id).await?;
        if from < 1 || to < 1 {
            return Err(AppError::InvalidArgument(
                "positions are 1-based and must be >= 1".into(),
            ));
        }
        let next = self.playlists.next_position(playlist_id).await?;
        // Clamp the destination into the live range so callers can pass a
        // large value to mean "the end".
        let to = to.min(next.saturating_sub(1).max(1));
        let before = self
            .playlists
            .get_track_at(playlist_id, from)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track at position {from}")))?;
        let moved = self
            .playlists
            .move_track(playlist_id, from, to)
            .await?;
        if !moved {
            return Err(AppError::NotFound(format!("track at position {from}")));
        }
        let after = PlaylistTrack {
            playlist_id,
            track_id: before.track_id,
            position: to,
            added_at: before.added_at,
        };
        self.audit(
            caller,
            "playlist.track.reorder",
            Some(playlist_id),
            Some(&before),
            Some(&after),
        )
        .await?;
        Ok(())
    }

    pub async fn list_tracks(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
    ) -> Result<Vec<PlaylistTrack>> {
        caller.require(PermissionLevel::User)?;
        // 404 the playlist explicitly so callers don't conflate "no tracks"
        // with "no playlist".
        if self.playlists.get(playlist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("playlist {playlist_id}")));
        }
        self.playlists.list_tracks(playlist_id).await
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity cannot own a playlist; log in as a user".into(),
            )
        })
    }

    /// Caller is `Manager+`, or caller's user_id == owner_id.
    fn caller_owns_or_manages(&self, caller: &Identity, owner_id: Uuid) -> bool {
        if caller.level().satisfies(PermissionLevel::Manager) {
            return true;
        }
        caller.user_id() == Some(owner_id)
    }

    async fn require_owner_or_manager(
        &self,
        caller: &Identity,
        playlist_id: Uuid,
    ) -> Result<Playlist> {
        caller.require(PermissionLevel::User)?;
        let p = self
            .playlists
            .get(playlist_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("playlist {playlist_id}")))?;
        self.require_owner_or_manager_on(caller, &p)?;
        Ok(p)
    }

    fn require_owner_or_manager_on(&self, caller: &Identity, p: &Playlist) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        if self.caller_owns_or_manages(caller, p.owner_id) {
            Ok(())
        } else {
            Err(AppError::PermissionDenied(
                "only the playlist owner or a Manager+ may modify it".into(),
            ))
        }
    }

    async fn audit<B: Serialize, A: Serialize>(
        &self,
        caller: &Identity,
        action: &str,
        entity_id: Option<Uuid>,
        before: Option<&B>,
        after: Option<&A>,
    ) -> Result<()> {
        let before_json = match before {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| AppError::Internal(format!("audit json: {e}")))?,
            ),
            None => None,
        };
        let after_json = match after {
            Some(v) => Some(
                serde_json::to_string(v)
                    .map_err(|e| AppError::Internal(format!("audit json: {e}")))?,
            ),
            None => None,
        };
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: action.to_string(),
                entity_type: "playlist".to_string(),
                entity_id,
                before_json,
                after_json,
            })
            .await?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidArgument(
            "playlist name is required".into(),
        ));
    }
    if trimmed.chars().count() > 200 {
        return Err(AppError::InvalidArgument(
            "playlist name must be <= 200 chars".into(),
        ));
    }
    Ok(name)
}

// Keep `m::Playlist` import live for future audit-row typing variants.
#[allow(dead_code)]
fn _force(_: m::Playlist) {}

#[cfg(test)]
mod tests {
    //! Unit tests against in-memory fake repos. The fakes implement enough
    //! of the trait surface to validate ownership rules, audit writes, and
    //! position-shift semantics without a live Postgres.

    use super::*;
    use crate::db::models::{
        AuditEntry, NewAuditEntry, NewPlaylist, NewTrack, NewUser, PermissionLevel, Playlist,
        PlaylistTrack, Track, User,
    };
    use crate::db::repo::{AuditRepo, PlaylistRepo, TrackRepo, UserRepo};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use time::OffsetDateTime;
    use uuid::Uuid;

    // ---- Playlist fake ----
    #[derive(Default)]
    struct FakePlaylists {
        playlists: Mutex<Vec<Playlist>>,
        tracks: Mutex<Vec<PlaylistTrack>>,
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[async_trait]
    impl PlaylistRepo for FakePlaylists {
        async fn create(&self, new: NewPlaylist) -> Result<Playlist> {
            let p = Playlist {
                id: Uuid::new_v4(),
                owner_id: new.owner_id,
                name: new.name,
                created_at: now(),
                updated_at: now(),
            };
            self.playlists.lock().unwrap().push(p.clone());
            Ok(p)
        }
        async fn get(&self, id: Uuid) -> Result<Option<Playlist>> {
            Ok(self.playlists.lock().unwrap().iter().find(|p| p.id == id).cloned())
        }
        async fn list_for_owner(&self, owner_id: Uuid) -> Result<Vec<Playlist>> {
            Ok(self
                .playlists
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.owner_id == owner_id)
                .cloned()
                .collect())
        }
        async fn update_name(&self, id: Uuid, name: &str) -> Result<Option<Playlist>> {
            let mut g = self.playlists.lock().unwrap();
            for p in g.iter_mut() {
                if p.id == id {
                    p.name = name.to_string();
                    p.updated_at = now();
                    return Ok(Some(p.clone()));
                }
            }
            Ok(None)
        }
        async fn delete(&self, id: Uuid) -> Result<()> {
            self.playlists.lock().unwrap().retain(|p| p.id != id);
            self.tracks.lock().unwrap().retain(|t| t.playlist_id != id);
            Ok(())
        }
        async fn insert_track_at(
            &self,
            playlist_id: Uuid,
            track_id: Uuid,
            position: i32,
        ) -> Result<PlaylistTrack> {
            let mut g = self.tracks.lock().unwrap();
            for t in g.iter_mut() {
                if t.playlist_id == playlist_id && t.position >= position {
                    t.position += 1;
                }
            }
            let row = PlaylistTrack {
                playlist_id,
                track_id,
                position,
                added_at: now(),
            };
            g.push(row.clone());
            Ok(row)
        }
        async fn remove_track_at(
            &self,
            playlist_id: Uuid,
            position: i32,
        ) -> Result<bool> {
            let mut g = self.tracks.lock().unwrap();
            let before = g.len();
            g.retain(|t| !(t.playlist_id == playlist_id && t.position == position));
            if g.len() == before {
                return Ok(false);
            }
            for t in g.iter_mut() {
                if t.playlist_id == playlist_id && t.position > position {
                    t.position -= 1;
                }
            }
            Ok(true)
        }
        async fn move_track(
            &self,
            playlist_id: Uuid,
            from: i32,
            to: i32,
        ) -> Result<bool> {
            let mut g = self.tracks.lock().unwrap();
            let Some(idx) = g
                .iter()
                .position(|t| t.playlist_id == playlist_id && t.position == from)
            else {
                return Ok(false);
            };
            if from == to {
                return Ok(true);
            }
            let track_id = g[idx].track_id;
            let added_at = g[idx].added_at;
            g.remove(idx);
            // shift in-between
            if from < to {
                for t in g.iter_mut() {
                    if t.playlist_id == playlist_id && t.position > from && t.position <= to {
                        t.position -= 1;
                    }
                }
            } else {
                for t in g.iter_mut() {
                    if t.playlist_id == playlist_id && t.position >= to && t.position < from {
                        t.position += 1;
                    }
                }
            }
            g.push(PlaylistTrack {
                playlist_id,
                track_id,
                position: to,
                added_at,
            });
            Ok(true)
        }
        async fn list_tracks(&self, playlist_id: Uuid) -> Result<Vec<PlaylistTrack>> {
            let mut rows: Vec<PlaylistTrack> = self
                .tracks
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.playlist_id == playlist_id)
                .cloned()
                .collect();
            rows.sort_by_key(|t| t.position);
            Ok(rows)
        }
        async fn next_position(&self, playlist_id: Uuid) -> Result<i32> {
            let n = self
                .tracks
                .lock()
                .unwrap()
                .iter()
                .filter(|t| t.playlist_id == playlist_id)
                .map(|t| t.position)
                .max();
            Ok(n.map(|m| m + 1).unwrap_or(1))
        }
        async fn get_track_at(
            &self,
            playlist_id: Uuid,
            position: i32,
        ) -> Result<Option<PlaylistTrack>> {
            Ok(self
                .tracks
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.playlist_id == playlist_id && t.position == position)
                .cloned())
        }
    }

    // ---- Minimal Track fake (only `get` is exercised) ----
    #[derive(Default)]
    struct FakeTracks {
        ids: Mutex<Vec<Uuid>>,
    }
    impl FakeTracks {
        fn add(&self, id: Uuid) {
            self.ids.lock().unwrap().push(id);
        }
        fn make_track(id: Uuid) -> Track {
            Track {
                id,
                album_id: Uuid::nil(),
                artist_id: Uuid::nil(),
                title: "t".into(),
                track_no: None,
                disc_no: None,
                duration_ms: 0,
                codec: "flac".into(),
                bitrate_kbps: None,
                file_path: format!("/fake/{id}.flac"),
                file_size: None,
                metadata_json: "{}".into(),
                created_at: now(),
                updated_at: now(),
            }
        }
    }
    #[async_trait]
    impl TrackRepo for FakeTracks {
        async fn create(&self, _: NewTrack) -> Result<Track> {
            unimplemented!()
        }
        async fn get(&self, id: Uuid) -> Result<Option<Track>> {
            Ok(self
                .ids
                .lock()
                .unwrap()
                .iter()
                .find(|i| **i == id)
                .map(|i| FakeTracks::make_track(*i)))
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
    }

    // ---- Stub user/audit ----
    #[derive(Default)]
    struct FakeUsers;
    #[async_trait]
    impl UserRepo for FakeUsers {
        async fn create(&self, _: NewUser) -> Result<User> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<User>> {
            Ok(None)
        }
        async fn find_by_username(&self, _: &str) -> Result<Option<User>> {
            Ok(None)
        }
        async fn update_permission(&self, _: Uuid, _: PermissionLevel) -> Result<()> {
            Ok(())
        }
        async fn update_password(&self, _: Uuid, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list(&self) -> Result<Vec<User>> {
            Ok(vec![])
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeAudit {
        entries: Mutex<Vec<NewAuditEntry>>,
    }
    #[async_trait]
    impl AuditRepo for FakeAudit {
        async fn record(&self, e: NewAuditEntry) -> Result<AuditEntry> {
            let row = AuditEntry {
                id: Uuid::new_v4(),
                actor_id: e.actor_id,
                action: e.action.clone(),
                entity_type: e.entity_type.clone(),
                entity_id: e.entity_id,
                before_json: e.before_json.clone(),
                after_json: e.after_json.clone(),
                created_at: now(),
            };
            self.entries.lock().unwrap().push(e);
            Ok(row)
        }
        async fn list_for_entity(
            &self,
            _: &str,
            _: Uuid,
        ) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }

    fn make_service() -> (
        PlaylistService,
        Arc<FakePlaylists>,
        Arc<FakeTracks>,
        Arc<FakeAudit>,
    ) {
        let pl = Arc::new(FakePlaylists::default());
        let tr = Arc::new(FakeTracks::default());
        let us = Arc::new(FakeUsers);
        let au = Arc::new(FakeAudit::default());
        let svc = PlaylistService::new(pl.clone(), tr.clone(), us, au.clone());
        (svc, pl, tr, au)
    }

    fn user_identity(level: PermissionLevel) -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level,
        }
    }

    #[tokio::test]
    async fn create_then_rename_and_audit() {
        let (svc, _pl, _tr, au) = make_service();
        let me = user_identity(PermissionLevel::User);
        let p = svc.create(&me, "  My Mix  ").await.unwrap();
        assert_eq!(p.name.trim(), "My Mix");
        let renamed = svc.rename(&me, p.id, "Better Mix").await.unwrap();
        assert_eq!(renamed.name, "Better Mix");
        let entries = au.entries.lock().unwrap();
        let actions: Vec<&str> = entries.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(actions, vec!["playlist.create", "playlist.update"]);
        // Every entry is tagged as a playlist.
        assert!(entries.iter().all(|e| e.entity_type == "playlist"));
    }

    #[tokio::test]
    async fn secret_key_cannot_own_playlist() {
        let (svc, ..) = make_service();
        let err = svc.create(&Identity::SecretKey, "x").await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn empty_name_rejected() {
        let (svc, ..) = make_service();
        let me = user_identity(PermissionLevel::User);
        let err = svc.create(&me, "   ").await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn non_owner_cannot_mutate_but_manager_can() {
        let (svc, ..) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let other = user_identity(PermissionLevel::User);
        let manager = user_identity(PermissionLevel::Manager);

        let p = svc.create(&owner, "shared").await.unwrap();

        // Non-owner user is denied.
        let err = svc.rename(&other, p.id, "hacked").await.unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));

        // Manager override works.
        svc.rename(&manager, p.id, "curated").await.unwrap();

        // Reads are open to any authed identity.
        svc.get(&other, p.id).await.unwrap();
    }

    #[tokio::test]
    async fn add_track_appends_and_shifts() {
        let (svc, pl, tr, _au) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let p = svc.create(&owner, "queue").await.unwrap();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let t3 = Uuid::new_v4();
        tr.add(t1);
        tr.add(t2);
        tr.add(t3);

        svc.add_track(&owner, p.id, t1).await.unwrap();
        svc.add_track(&owner, p.id, t2).await.unwrap();
        // Insert in front; t1 -> pos 2, t2 -> pos 3.
        svc.insert_track(&owner, p.id, t3, 1).await.unwrap();

        let rows = pl.list_tracks(p.id).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.position, r.track_id)).collect::<Vec<_>>(),
            vec![(1, t3), (2, t1), (3, t2)]
        );
    }

    #[tokio::test]
    async fn add_unknown_track_is_404() {
        let (svc, ..) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let p = svc.create(&owner, "x").await.unwrap();
        let err = svc.add_track(&owner, p.id, Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn reorder_forward_and_backward() {
        let (svc, pl, tr, _au) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let p = svc.create(&owner, "x").await.unwrap();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        for id in &ids {
            tr.add(*id);
            svc.add_track(&owner, p.id, *id).await.unwrap();
        }

        // Move position 1 -> 3: order becomes [id1, id2, id0, id3]
        svc.reorder(&owner, p.id, 1, 3).await.unwrap();
        let rows = pl.list_tracks(p.id).await.unwrap();
        let ordered: Vec<Uuid> = rows.iter().map(|r| r.track_id).collect();
        assert_eq!(ordered, vec![ids[1], ids[2], ids[0], ids[3]]);

        // Move position 4 -> 1: order becomes [id3, id1, id2, id0]
        svc.reorder(&owner, p.id, 4, 1).await.unwrap();
        let rows = pl.list_tracks(p.id).await.unwrap();
        let ordered: Vec<Uuid> = rows.iter().map(|r| r.track_id).collect();
        assert_eq!(ordered, vec![ids[3], ids[1], ids[2], ids[0]]);
    }

    #[tokio::test]
    async fn remove_shifts_subsequent_positions_down() {
        let (svc, pl, tr, _au) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let p = svc.create(&owner, "x").await.unwrap();
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for id in &ids {
            tr.add(*id);
            svc.add_track(&owner, p.id, *id).await.unwrap();
        }
        let removed = svc
            .remove_track_at(&owner, p.id, 2)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(removed.track_id, ids[1]);
        let rows = pl.list_tracks(p.id).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| (r.position, r.track_id)).collect::<Vec<_>>(),
            vec![(1, ids[0]), (2, ids[2])]
        );
    }

    #[tokio::test]
    async fn list_for_owner_blocked_for_other_users() {
        let (svc, ..) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let other = user_identity(PermissionLevel::User);
        let owner_id = owner.user_id().unwrap();
        svc.create(&owner, "x").await.unwrap();
        let err = svc.list_for_owner(&other, owner_id).await.unwrap_err();
        assert!(matches!(err, AppError::PermissionDenied(_)));
        // The owner sees their own list.
        svc.list_for_owner(&owner, owner_id).await.unwrap();
    }

    #[tokio::test]
    async fn delete_clears_audit_and_returns_false_when_missing() {
        let (svc, _pl, _tr, au) = make_service();
        let owner = user_identity(PermissionLevel::User);
        let p = svc.create(&owner, "x").await.unwrap();
        assert!(svc.delete(&owner, p.id).await.unwrap());
        assert!(!svc.delete(&owner, p.id).await.unwrap());
        let actions: Vec<String> = au
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.action.clone())
            .collect();
        assert_eq!(actions, vec!["playlist.create", "playlist.delete"]);
    }
}
