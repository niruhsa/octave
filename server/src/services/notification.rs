//! Follows & notifications (Phase 10).
//!
//! Two related concerns served by one service:
//! - **Follows:** a user follows/unfollows an artist (`follows` table). Any
//!   authed *user* may follow (the `SECRET_KEY` identity has no `user_id`, so
//!   it is rejected — there's no user to own the follow). Follow/unfollow are
//!   audited (`artist.follow` / `artist.unfollow`).
//! - **Notifications:** when a new release is added for a followed artist, one
//!   notification row is persisted per follower (the new-release **fan-out**,
//!   driven from [`LibraryService::create_album`](crate::services::LibraryService)).
//!   Clients fetch + mark them read. Delivery is persist-then-fetch; a push
//!   transport can be layered on later.
//!
//! A "release" is an **album** — the fan-out fires once when an album is first
//! created (manually or by ingest), not per track, so followers get one alert
//! per release rather than one per song.

use std::sync::Arc;

use uuid::Uuid;

use tracing::warn;

use crate::auth::Identity;
use crate::db::models::{
    Album, Artist, NewAuditEntry, NewDeviceToken, NewNotification, Notification, PermissionLevel,
    Podcast, PodcastEpisode,
};
use crate::db::repo::{ArtistRepo, AuditRepo, DeviceTokenRepo, FollowRepo, NotificationRepo};
use crate::error::{AppError, Result};
use crate::services::fcm::{PushOutcome, PushSender};

const MAX_PAGE_LIMIT: i64 = 200;
const DEFAULT_PAGE_LIMIT: i64 = 50;

/// Notification `kind` for a new release by a followed artist.
const KIND_NEW_RELEASE: &str = "new_release";
/// Notification `kind` for a new episode of a subscribed podcast.
const KIND_NEW_EPISODE: &str = "new_episode";

#[derive(Clone)]
pub struct NotificationService {
    pub follows: Arc<dyn FollowRepo>,
    pub notifications: Arc<dyn NotificationRepo>,
    /// Artists are read for follow validation, follow-list enrichment, and the
    /// new-release notification title.
    pub artists: Arc<dyn ArtistRepo>,
    pub audit: Arc<dyn AuditRepo>,
    /// Registered device push tokens (per follower) for the FCM fan-out.
    pub device_tokens: Arc<dyn DeviceTokenRepo>,
    /// Optional push backend (FCM). `None` when push is disabled — the client
    /// then relies on polling. Set via [`with_push`](Self::with_push).
    pub push: Option<Arc<dyn PushSender>>,
}

impl NotificationService {
    pub fn new(
        follows: Arc<dyn FollowRepo>,
        notifications: Arc<dyn NotificationRepo>,
        artists: Arc<dyn ArtistRepo>,
        audit: Arc<dyn AuditRepo>,
        device_tokens: Arc<dyn DeviceTokenRepo>,
    ) -> Self {
        Self {
            follows,
            notifications,
            artists,
            audit,
            device_tokens,
            push: None,
        }
    }

    /// Wire in a push backend so the new-release fan-out also delivers a
    /// real-time push to followers' registered devices (Phase 10 — FCM).
    pub fn with_push(mut self, push: Option<Arc<dyn PushSender>>) -> Self {
        self.push = push;
        self
    }

    // -----------------------------------------------------------------------
    // Device tokens (push registration)
    // -----------------------------------------------------------------------

    /// Register (or re-own) a device push token for the caller. Any authed
    /// user; `SECRET_KEY` rejected (no user to own the token).
    pub async fn register_device(
        &self,
        caller: &Identity,
        token: &str,
        platform: &str,
    ) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        if token.trim().is_empty() {
            return Err(AppError::InvalidArgument("device token is required".into()));
        }
        let platform = if platform.trim().is_empty() {
            "android".to_string()
        } else {
            platform.trim().to_string()
        };
        self.device_tokens
            .upsert(NewDeviceToken {
                token: token.to_string(),
                user_id,
                platform,
            })
            .await?;
        Ok(())
    }

    /// Unregister a device push token (logout). Any authed user; idempotent.
    pub async fn unregister_device(&self, caller: &Identity, token: &str) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        self.caller_user_id(caller)?;
        self.device_tokens.delete(token).await
    }

    // -----------------------------------------------------------------------
    // Follows
    // -----------------------------------------------------------------------

    /// Follow an artist. Idempotent (the repo's insert is `ON CONFLICT DO
    /// NOTHING`). Any authed user; `SECRET_KEY` rejected. Audited.
    pub async fn follow(&self, caller: &Identity, artist_id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        if self.artists.get(artist_id).await?.is_none() {
            return Err(AppError::NotFound(format!("artist {artist_id}")));
        }
        self.follows.follow(user_id, artist_id).await?;
        self.audit(
            caller,
            "artist.follow",
            artist_id,
            None,
            Some(serde_json::json!({ "user_id": user_id, "artist_id": artist_id })),
        )
        .await?;
        Ok(())
    }

    /// Unfollow an artist. Idempotent. Any authed user; `SECRET_KEY` rejected.
    /// Audited.
    pub async fn unfollow(&self, caller: &Identity, artist_id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.follows.unfollow(user_id, artist_id).await?;
        self.audit(
            caller,
            "artist.unfollow",
            artist_id,
            Some(serde_json::json!({ "user_id": user_id, "artist_id": artist_id })),
            None,
        )
        .await?;
        Ok(())
    }

    /// The artists the caller follows (full rows, name-resolved). `SECRET_KEY`
    /// rejected. Follow counts are small per user, so this resolves each row
    /// individually rather than adding a batch repo read.
    pub async fn list_following(&self, caller: &Identity) -> Result<Vec<Artist>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let ids = self.follows.following(user_id).await?;
        let mut artists = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(a) = self.artists.get(id).await? {
                artists.push(a);
            }
        }
        Ok(artists)
    }

    /// Whether the caller follows `artist_id`. Cheap UI helper for an artist
    /// page's follow toggle. `SECRET_KEY` rejected.
    pub async fn is_following(&self, caller: &Identity, artist_id: Uuid) -> Result<bool> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        Ok(self.follows.following(user_id).await?.contains(&artist_id))
    }

    // -----------------------------------------------------------------------
    // New-release fan-out (called from LibraryService::create_album)
    // -----------------------------------------------------------------------

    /// Notify every follower of `artist_id` about a newly-created `album`.
    /// `actor` (the user who added the release, if any) is excluded so an
    /// uploader who happens to follow the artist isn't alerted to their own
    /// action. Returns the number of notifications created.
    ///
    /// Best-effort by contract: the caller (`create_album`) logs and swallows
    /// any error so a notification failure never fails the album creation.
    pub async fn notify_new_release(
        &self,
        actor: Option<Uuid>,
        artist_id: Uuid,
        album: &Album,
    ) -> Result<u64> {
        let followers = self.follows.followers_of(artist_id).await?;
        if followers.is_empty() {
            return Ok(0);
        }
        let artist_name = match self.artists.get(artist_id).await? {
            Some(a) => a.name,
            None => "an artist you follow".to_string(),
        };
        let title = format!("New release from {artist_name}");
        let body_text = album.title.clone();

        let recipients: Vec<Uuid> = followers
            .into_iter()
            .filter(|uid| Some(*uid) != actor)
            .collect();
        if recipients.is_empty() {
            return Ok(0);
        }
        let items: Vec<NewNotification> = recipients
            .iter()
            .map(|uid| NewNotification {
                user_id: *uid,
                kind: KIND_NEW_RELEASE.to_string(),
                artist_id: Some(artist_id),
                album_id: Some(album.id),
                title: title.clone(),
                body: Some(body_text.clone()),
                ..Default::default()
            })
            .collect();
        let created = self.notifications.create_many(&items).await?;

        // Real-time push to followers' devices (FCM). Best-effort + in the
        // background: a slow/failing push must never delay or fail album
        // creation. Persisted notifications above are the source of truth; push
        // is the delivery channel, and the client also polls as a fallback.
        if self.push.is_some() {
            let this = self.clone();
            // Data payload for tap-routing on the device.
            let data = vec![
                ("kind".to_string(), KIND_NEW_RELEASE.to_string()),
                ("artist_id".to_string(), artist_id.to_string()),
                ("album_id".to_string(), album.id.to_string()),
            ];
            tokio::spawn(async move {
                this.push_fanout(&recipients, &title, &body_text, &data).await;
            });
        }
        Ok(created)
    }

    /// Notify every follower of `artist_id` about a release **detected on the
    /// metadata provider** (MusicBrainz) that the library doesn't have — the
    /// discography-sync counterpart to [`notify_new_release`], which fires on a
    /// *library* addition (Phase D). There's no actor to exclude (the release
    /// comes from the provider, not a user action), so it fans out to every
    /// follower. `album_id` is left `None` (there's no local album yet), so a
    /// notification tap routes to the artist page. Best-effort by contract:
    /// the caller (the discography sync) logs and swallows any error so a
    /// notification failure never fails the sync. Returns the number created.
    pub async fn notify_provider_release(&self, artist_id: Uuid, title: &str) -> Result<u64> {
        let followers = self.follows.followers_of(artist_id).await?;
        if followers.is_empty() {
            return Ok(0);
        }
        let artist_name = match self.artists.get(artist_id).await? {
            Some(a) => a.name,
            None => "an artist you follow".to_string(),
        };
        let heading = format!("New release from {artist_name}");
        let body_text = title.to_string();
        let items: Vec<NewNotification> = followers
            .iter()
            .map(|uid| NewNotification {
                user_id: *uid,
                kind: KIND_NEW_RELEASE.to_string(),
                artist_id: Some(artist_id),
                title: heading.clone(),
                body: Some(body_text.clone()),
                ..Default::default()
            })
            .collect();
        let created = self.notifications.create_many(&items).await?;

        // Real-time push (best-effort, background) — same pattern as
        // `notify_new_release`, but the tap-routing data carries only the
        // artist (no album exists yet).
        if self.push.is_some() {
            let this = self.clone();
            let recipients = followers.clone();
            let data = vec![
                ("kind".to_string(), KIND_NEW_RELEASE.to_string()),
                ("artist_id".to_string(), artist_id.to_string()),
            ];
            tokio::spawn(async move {
                this.push_fanout(&recipients, &heading, &body_text, &data).await;
            });
        }
        Ok(created)
    }

    /// Notify every subscriber of `podcast` about a newly-published `episode`.
    /// Best-effort fan-out (same contract as [`notify_new_release`]); returns
    /// the number of notifications created. Unlike new releases there's no actor
    /// to exclude — episodes come from the feed, not a user action, so the
    /// caller passes the full subscriber list.
    ///
    /// [`notify_new_release`]: Self::notify_new_release
    pub async fn notify_new_episode(
        &self,
        subscribers: &[Uuid],
        podcast: &Podcast,
        episode: &PodcastEpisode,
    ) -> Result<u64> {
        if subscribers.is_empty() {
            return Ok(0);
        }
        let title = format!("New episode of {}", podcast.title);
        let body_text = episode.title.clone();
        let items: Vec<NewNotification> = subscribers
            .iter()
            .map(|uid| NewNotification {
                user_id: *uid,
                kind: KIND_NEW_EPISODE.to_string(),
                podcast_id: Some(podcast.id),
                episode_id: Some(episode.id),
                title: title.clone(),
                body: Some(body_text.clone()),
                ..Default::default()
            })
            .collect();
        let created = self.notifications.create_many(&items).await?;

        if self.push.is_some() {
            let this = self.clone();
            let recipients = subscribers.to_vec();
            let data = vec![
                ("kind".to_string(), KIND_NEW_EPISODE.to_string()),
                ("podcast_id".to_string(), podcast.id.to_string()),
                ("episode_id".to_string(), episode.id.to_string()),
            ];
            tokio::spawn(async move {
                this.push_fanout(&recipients, &title, &body_text, &data).await;
            });
        }
        Ok(created)
    }

    /// Push a notification to every registered device of each recipient, with a
    /// `data` payload for tap-routing. Prunes tokens FCM reports as
    /// unregistered. Best-effort: all errors are logged, never propagated.
    async fn push_fanout(
        &self,
        recipients: &[Uuid],
        title: &str,
        body: &str,
        data: &[(String, String)],
    ) {
        let Some(push) = &self.push else {
            return;
        };
        for uid in recipients {
            let tokens = match self.device_tokens.list_for_user(*uid).await {
                Ok(t) => t,
                Err(e) => {
                    warn!(user_id = %uid, error = %e, "push: list device tokens failed");
                    continue;
                }
            };
            for dt in tokens {
                match push.send(&dt.token, title, body, data).await {
                    Ok(PushOutcome::Delivered) => {}
                    Ok(PushOutcome::Unregistered) => {
                        // Dead token — prune so we stop trying it.
                        if let Err(e) = self.device_tokens.delete(&dt.token).await {
                            warn!(error = %e, "push: prune unregistered token failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "push: fcm send failed"),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Notification reads
    // -----------------------------------------------------------------------

    /// A page of the caller's notifications, newest first. `SECRET_KEY`
    /// rejected (notifications are per-user).
    pub async fn list_notifications(
        &self,
        caller: &Identity,
        unread_only: bool,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Notification>> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let (limit, offset) = paginate(limit, offset);
        self.notifications
            .list_for_user(user_id, unread_only, limit, offset)
            .await
    }

    /// The caller's unread notification count (for a UI badge).
    pub async fn unread_count(&self, caller: &Identity) -> Result<i64> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.notifications.unread_count(user_id).await
    }

    /// Mark one notification read. 404 when it doesn't exist or belongs to
    /// another user (existence is not leaked). Already-read is a success no-op.
    pub async fn mark_read(&self, caller: &Identity, id: Uuid) -> Result<()> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        let n = self
            .notifications
            .get(id)
            .await?
            .filter(|n| n.user_id == user_id)
            .ok_or_else(|| AppError::NotFound(format!("notification {id}")))?;
        // Already read → nothing to do. Otherwise flip it (scoped to the user).
        if n.read_at.is_none() {
            self.notifications.mark_read(user_id, id).await?;
        }
        Ok(())
    }

    /// Mark all the caller's notifications read. Returns the count flipped.
    pub async fn mark_all_read(&self, caller: &Identity) -> Result<u64> {
        caller.require(PermissionLevel::User)?;
        let user_id = self.caller_user_id(caller)?;
        self.notifications.mark_all_read(user_id).await
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn caller_user_id(&self, caller: &Identity) -> Result<Uuid> {
        caller.user_id().ok_or_else(|| {
            AppError::InvalidArgument(
                "SECRET_KEY identity has no user to follow or receive notifications; \
                 log in as a user"
                    .into(),
            )
        })
    }

    async fn audit(
        &self,
        caller: &Identity,
        action: &str,
        artist_id: Uuid,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> Result<()> {
        let to_json = |v: Option<serde_json::Value>| -> Result<Option<String>> {
            match v {
                Some(v) => {
                    Ok(Some(serde_json::to_string(&v).map_err(|e| {
                        AppError::Internal(format!("audit json: {e}"))
                    })?))
                }
                None => Ok(None),
            }
        };
        self.audit
            .record(NewAuditEntry {
                actor_id: caller.user_id(),
                action: action.to_string(),
                entity_type: "artist".to_string(),
                entity_id: Some(artist_id),
                before_json: to_json(before)?,
                after_json: to_json(after)?,
            })
            .await?;
        Ok(())
    }
}

fn paginate(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
    let offset = offset.unwrap_or(0).max(0);
    (limit, offset)
}

// Surface the rarely-used `is_following` read to callers without a warning if
// no transport wires it up yet.
#[allow(dead_code)]
fn _force(_: &NotificationService) {}

#[cfg(test)]
mod tests {
    //! Unit tests against in-memory fakes (no live Postgres). They validate the
    //! follow permission rules + audit writes, the new-release fan-out
    //! (follower selection + actor exclusion), and the read/ownership semantics.

    use super::*;
    use crate::db::models::{
        AuditEntry, DeviceToken, NewArtist, NewNotification, NewUser, PermissionLevel, Podcast,
        PodcastEpisode, User,
    };
    use crate::db::repo::{
        ArtistRepo, AuditRepo, DeviceTokenRepo, FollowRepo, NotificationRepo, TrackIdPath,
    };
    use crate::services::fcm::{PushOutcome, PushSender};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use time::OffsetDateTime;

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    // ---- Follows fake ----
    #[derive(Default)]
    struct FakeFollows {
        // (user_id, artist_id)
        rows: Mutex<Vec<(Uuid, Uuid)>>,
    }
    #[async_trait]
    impl FollowRepo for FakeFollows {
        async fn follow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()> {
            let mut g = self.rows.lock().unwrap();
            if !g.iter().any(|(u, a)| *u == user_id && *a == artist_id) {
                g.push((user_id, artist_id));
            }
            Ok(())
        }
        async fn unfollow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()> {
            self.rows
                .lock()
                .unwrap()
                .retain(|(u, a)| !(*u == user_id && *a == artist_id));
            Ok(())
        }
        async fn followers_of(&self, artist_id: Uuid) -> Result<Vec<Uuid>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, a)| *a == artist_id)
                .map(|(u, _)| *u)
                .collect())
        }
        async fn following(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|(u, _)| *u == user_id)
                .map(|(_, a)| *a)
                .collect())
        }
        async fn reassign_artist(&self, _from: Uuid, _to: Uuid) -> Result<()> {
            Ok(())
        }
    }

    // ---- Notifications fake ----
    #[derive(Default)]
    struct FakeNotifications {
        rows: Mutex<Vec<Notification>>,
    }
    impl FakeNotifications {
        fn build(new: &NewNotification) -> Notification {
            Notification {
                id: Uuid::new_v4(),
                user_id: new.user_id,
                kind: new.kind.clone(),
                artist_id: new.artist_id,
                album_id: new.album_id,
                podcast_id: new.podcast_id,
                episode_id: new.episode_id,
                title: new.title.clone(),
                body: new.body.clone(),
                read_at: None,
                created_at: now(),
            }
        }
    }
    #[async_trait]
    impl NotificationRepo for FakeNotifications {
        async fn create(&self, new: NewNotification) -> Result<Notification> {
            let row = Self::build(&new);
            self.rows.lock().unwrap().push(row.clone());
            Ok(row)
        }
        async fn create_many(&self, items: &[NewNotification]) -> Result<u64> {
            let mut g = self.rows.lock().unwrap();
            for it in items {
                g.push(Self::build(it));
            }
            Ok(items.len() as u64)
        }
        async fn get(&self, id: Uuid) -> Result<Option<Notification>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|n| n.id == id)
                .cloned())
        }
        async fn list_for_user(
            &self,
            user_id: Uuid,
            unread_only: bool,
            limit: i64,
            offset: i64,
        ) -> Result<Vec<Notification>> {
            let mut rows: Vec<Notification> = self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.user_id == user_id && (!unread_only || n.read_at.is_none()))
                .cloned()
                .collect();
            rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Ok(rows
                .into_iter()
                .skip(offset.max(0) as usize)
                .take(limit.max(0) as usize)
                .collect())
        }
        async fn unread_count(&self, user_id: Uuid) -> Result<i64> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|n| n.user_id == user_id && n.read_at.is_none())
                .count() as i64)
        }
        async fn mark_read(&self, user_id: Uuid, id: Uuid) -> Result<bool> {
            let mut g = self.rows.lock().unwrap();
            for n in g.iter_mut() {
                if n.id == id && n.user_id == user_id && n.read_at.is_none() {
                    n.read_at = Some(now());
                    return Ok(true);
                }
            }
            Ok(false)
        }
        async fn mark_all_read(&self, user_id: Uuid) -> Result<u64> {
            let mut g = self.rows.lock().unwrap();
            let mut count = 0;
            for n in g.iter_mut() {
                if n.user_id == user_id && n.read_at.is_none() {
                    n.read_at = Some(now());
                    count += 1;
                }
            }
            Ok(count)
        }
    }

    // ---- Artists fake (only get/create exercised) ----
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
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.id == id)
                .cloned())
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
        async fn find_by_name(&self, name: &str) -> Result<Option<Artist>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|a| a.name == name)
                .cloned())
        }
        async fn delete(&self, _: Uuid) -> Result<()> {
            Ok(())
        }
    }

    // ---- Audit fake ----
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
        async fn list_for_entity(&self, _: &str, _: Uuid) -> Result<Vec<AuditEntry>> {
            Ok(vec![])
        }
    }

    // ---- Device-token fake ----
    #[derive(Default)]
    struct FakeDeviceTokens {
        rows: Mutex<Vec<DeviceToken>>,
    }
    impl FakeDeviceTokens {
        fn add(&self, user_id: Uuid, token: &str) {
            self.rows.lock().unwrap().push(DeviceToken {
                token: token.to_string(),
                user_id,
                platform: "android".into(),
                created_at: now(),
                last_seen_at: now(),
            });
        }
        fn tokens_for(&self, user_id: Uuid) -> Vec<String> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .filter(|d| d.user_id == user_id)
                .map(|d| d.token.clone())
                .collect()
        }
    }
    #[async_trait]
    impl DeviceTokenRepo for FakeDeviceTokens {
        async fn upsert(&self, new: NewDeviceToken) -> Result<DeviceToken> {
            let mut g = self.rows.lock().unwrap();
            g.retain(|d| d.token != new.token); // re-own on conflict
            let row = DeviceToken {
                token: new.token,
                user_id: new.user_id,
                platform: new.platform,
                created_at: now(),
                last_seen_at: now(),
            };
            g.push(row.clone());
            Ok(row)
        }
        async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<DeviceToken>> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .filter(|d| d.user_id == user_id)
                .cloned()
                .collect())
        }
        async fn delete(&self, token: &str) -> Result<()> {
            self.rows.lock().unwrap().retain(|d| d.token != token);
            Ok(())
        }
    }

    // ---- Push-sender fake ----
    #[derive(Default)]
    struct FakePush {
        sent: Mutex<Vec<String>>,
        unregistered: Mutex<std::collections::HashSet<String>>,
    }
    impl FakePush {
        fn set_unregistered(&self, token: &str) {
            self.unregistered.lock().unwrap().insert(token.to_string());
        }
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }
    #[async_trait]
    impl PushSender for FakePush {
        async fn send(
            &self,
            token: &str,
            _title: &str,
            _body: &str,
            _data: &[(String, String)],
        ) -> Result<PushOutcome> {
            self.sent.lock().unwrap().push(token.to_string());
            if self.unregistered.lock().unwrap().contains(token) {
                Ok(PushOutcome::Unregistered)
            } else {
                Ok(PushOutcome::Delivered)
            }
        }
    }

    // Keep the unused imports honest (User/NewUser referenced by trait sigs only).
    #[allow(dead_code)]
    fn _types(_: User, _: NewUser, _: TrackIdPath) {}

    fn make_service() -> (
        NotificationService,
        Arc<FakeFollows>,
        Arc<FakeNotifications>,
        Arc<FakeArtists>,
        Arc<FakeAudit>,
    ) {
        let follows = Arc::new(FakeFollows::default());
        let notifs = Arc::new(FakeNotifications::default());
        let artists = Arc::new(FakeArtists::default());
        let audit = Arc::new(FakeAudit::default());
        let devices = Arc::new(FakeDeviceTokens::default());
        let svc = NotificationService::new(
            follows.clone(),
            notifs.clone(),
            artists.clone(),
            audit.clone(),
            devices,
        );
        (svc, follows, notifs, artists, audit)
    }

    /// Variant exposing the device-token + push fakes for the FCM tests.
    fn make_service_with_push() -> (
        NotificationService,
        Arc<FakeFollows>,
        Arc<FakeArtists>,
        Arc<FakeDeviceTokens>,
        Arc<FakePush>,
    ) {
        let follows = Arc::new(FakeFollows::default());
        let notifs = Arc::new(FakeNotifications::default());
        let artists = Arc::new(FakeArtists::default());
        let audit = Arc::new(FakeAudit::default());
        let devices = Arc::new(FakeDeviceTokens::default());
        let push = Arc::new(FakePush::default());
        let svc = NotificationService::new(
            follows.clone(),
            notifs,
            artists.clone(),
            audit,
            devices.clone(),
        )
        .with_push(Some(push.clone()));
        (svc, follows, artists, devices, push)
    }

    fn user(level: PermissionLevel) -> Identity {
        Identity::User {
            id: Uuid::new_v4(),
            username: "u".into(),
            level,
        }
    }

    fn album(artist_id: Uuid, title: &str) -> Album {
        Album {
            id: Uuid::new_v4(),
            artist_id,
            title: title.to_string(),
            release_year: Some(2026),
            album_type: "album".into(),
            is_explicit: false,
            cover_path: None,
            storage_bytes: 0,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn podcast(title: &str) -> Podcast {
        Podcast {
            id: Uuid::new_v4(),
            feed_url: "https://feeds.example.com/show".into(),
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
        }
    }

    fn episode(podcast_id: Uuid, title: &str) -> PodcastEpisode {
        PodcastEpisode {
            id: Uuid::new_v4(),
            podcast_id,
            guid: Uuid::new_v4().to_string(),
            title: title.to_string(),
            description: None,
            enclosure_url: "https://cdn.example.com/ep.mp3".into(),
            enclosure_type: Some("audio/mpeg".into()),
            episode_no: None,
            season_no: None,
            duration_ms: None,
            codec: None,
            bitrate_kbps: None,
            file_path: None,
            file_size: None,
            image_path: None,
            published_at: None,
            metadata_json: "{}".into(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[tokio::test]
    async fn follow_then_list_and_audit() {
        let (svc, _f, _n, artists, audit) = make_service();
        let me = user(PermissionLevel::User);
        let artist = artists.insert("BABYMETAL");

        svc.follow(&me, artist.id).await.unwrap();
        // Idempotent — a second follow doesn't duplicate.
        svc.follow(&me, artist.id).await.unwrap();

        let following = svc.list_following(&me).await.unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0].id, artist.id);
        assert!(svc.is_following(&me, artist.id).await.unwrap());

        let actions: Vec<String> = audit
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.action.clone())
            .collect();
        assert_eq!(actions, vec!["artist.follow", "artist.follow"]);
    }

    #[tokio::test]
    async fn unfollow_removes() {
        let (svc, _f, _n, artists, _au) = make_service();
        let me = user(PermissionLevel::User);
        let artist = artists.insert("ROSÉ");
        svc.follow(&me, artist.id).await.unwrap();
        svc.unfollow(&me, artist.id).await.unwrap();
        assert!(svc.list_following(&me).await.unwrap().is_empty());
        assert!(!svc.is_following(&me, artist.id).await.unwrap());
    }

    #[tokio::test]
    async fn follow_unknown_artist_is_404() {
        let (svc, ..) = make_service();
        let me = user(PermissionLevel::User);
        let err = svc.follow(&me, Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn secret_key_cannot_follow_or_list() {
        let (svc, _f, _n, artists, _au) = make_service();
        let artist = artists.insert("a");
        let err = svc
            .follow(&Identity::SecretKey, artist.id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        let err = svc
            .list_notifications(&Identity::SecretKey, false, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn new_release_fans_out_to_followers_excluding_actor() {
        let (svc, _f, notifs, artists, _au) = make_service();
        let artist = artists.insert("Hearts2Hearts");

        let fan1 = user(PermissionLevel::User);
        let fan2 = user(PermissionLevel::User);
        let manager = user(PermissionLevel::Manager); // also a follower + the uploader
        svc.follow(&fan1, artist.id).await.unwrap();
        svc.follow(&fan2, artist.id).await.unwrap();
        svc.follow(&manager, artist.id).await.unwrap();

        // The manager uploads the release → actor excluded from the fan-out.
        let release = album(artist.id, "The Chase");
        let created = svc
            .notify_new_release(manager.user_id(), artist.id, &release)
            .await
            .unwrap();
        assert_eq!(created, 2, "both other followers, not the actor");

        // Each follower sees exactly one unread notification; the actor none.
        assert_eq!(svc.unread_count(&fan1).await.unwrap(), 1);
        assert_eq!(svc.unread_count(&fan2).await.unwrap(), 1);
        assert_eq!(svc.unread_count(&manager).await.unwrap(), 0);

        let n = &svc
            .list_notifications(&fan1, true, None, None)
            .await
            .unwrap()[0];
        assert_eq!(n.kind, "new_release");
        assert_eq!(n.album_id, Some(release.id));
        assert_eq!(n.artist_id, Some(artist.id));
        assert!(n.title.contains("Hearts2Hearts"));
        assert_eq!(n.body.as_deref(), Some("The Chase"));
        // Sanity: no stray rows were created.
        assert_eq!(notifs.rows.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn new_episode_fans_out_to_subscribers() {
        let (svc, _f, notifs, _artists, _au) = make_service();
        let sub1 = user(PermissionLevel::User);
        let sub2 = user(PermissionLevel::User);
        let pod = podcast("Daily Tech");
        let ep = episode(pod.id, "Episode 42");

        let recipients = [sub1.user_id().unwrap(), sub2.user_id().unwrap()];
        let created = svc.notify_new_episode(&recipients, &pod, &ep).await.unwrap();
        assert_eq!(created, 2);

        assert_eq!(svc.unread_count(&sub1).await.unwrap(), 1);
        assert_eq!(svc.unread_count(&sub2).await.unwrap(), 1);

        let n = &svc.list_notifications(&sub1, true, None, None).await.unwrap()[0];
        assert_eq!(n.kind, "new_episode");
        assert_eq!(n.podcast_id, Some(pod.id));
        assert_eq!(n.episode_id, Some(ep.id));
        assert!(n.artist_id.is_none() && n.album_id.is_none());
        assert!(n.title.contains("Daily Tech"));
        assert_eq!(n.body.as_deref(), Some("Episode 42"));
        assert_eq!(notifs.rows.lock().unwrap().len(), 2);

        // Empty subscriber list → no-op.
        assert_eq!(svc.notify_new_episode(&[], &pod, &ep).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn new_release_with_no_followers_is_noop() {
        let (svc, _f, _n, artists, _au) = make_service();
        let artist = artists.insert("Nobody Follows");
        let release = album(artist.id, "Empty");
        let created = svc
            .notify_new_release(None, artist.id, &release)
            .await
            .unwrap();
        assert_eq!(created, 0);
    }

    #[tokio::test]
    async fn mark_read_and_mark_all_read() {
        let (svc, _f, _n, artists, _au) = make_service();
        let artist = artists.insert("a");
        let fan = user(PermissionLevel::User);
        svc.follow(&fan, artist.id).await.unwrap();

        // Two releases → two unread notifications.
        svc.notify_new_release(None, artist.id, &album(artist.id, "A"))
            .await
            .unwrap();
        svc.notify_new_release(None, artist.id, &album(artist.id, "B"))
            .await
            .unwrap();
        assert_eq!(svc.unread_count(&fan).await.unwrap(), 2);

        let first = svc
            .list_notifications(&fan, false, None, None)
            .await
            .unwrap()[0]
            .id;
        svc.mark_read(&fan, first).await.unwrap();
        assert_eq!(svc.unread_count(&fan).await.unwrap(), 1);
        // Re-marking an already-read notification is a no-op success.
        svc.mark_read(&fan, first).await.unwrap();
        assert_eq!(svc.unread_count(&fan).await.unwrap(), 1);

        let flipped = svc.mark_all_read(&fan).await.unwrap();
        assert_eq!(flipped, 1);
        assert_eq!(svc.unread_count(&fan).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cannot_read_another_users_notification() {
        let (svc, _f, _n, artists, _au) = make_service();
        let artist = artists.insert("a");
        let owner = user(PermissionLevel::User);
        let other = user(PermissionLevel::User);
        svc.follow(&owner, artist.id).await.unwrap();
        svc.notify_new_release(None, artist.id, &album(artist.id, "A"))
            .await
            .unwrap();

        let id = svc
            .list_notifications(&owner, false, None, None)
            .await
            .unwrap()[0]
            .id;
        // Another user can't mark it read — and existence isn't leaked (404).
        let err = svc.mark_read(&other, id).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
        // Owner's notification is untouched.
        assert_eq!(svc.unread_count(&owner).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn register_device_gated_and_upserts() {
        let (svc, _f, _a, devices, _p) = make_service_with_push();
        // SECRET_KEY has no user to own a token → rejected.
        let err = svc
            .register_device(&Identity::SecretKey, "tok", "android")
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        // Empty token rejected.
        let me = user(PermissionLevel::User);
        let err = svc.register_device(&me, "  ", "android").await.unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
        // A real user registers + the token lands; unregister removes it.
        svc.register_device(&me, "tok-1", "android").await.unwrap();
        assert_eq!(devices.tokens_for(me.user_id().unwrap()), vec!["tok-1"]);
        svc.unregister_device(&me, "tok-1").await.unwrap();
        assert!(devices.tokens_for(me.user_id().unwrap()).is_empty());
    }

    #[tokio::test]
    async fn push_fanout_delivers_to_devices_and_prunes_unregistered() {
        let (svc, follows, artists, devices, push) = make_service_with_push();
        let artist = artists.insert("Hearts2Hearts");
        let fan1 = user(PermissionLevel::User);
        let fan2 = user(PermissionLevel::User);
        let (u1, u2) = (fan1.user_id().unwrap(), fan2.user_id().unwrap());
        follows.follow(u1, artist.id).await.unwrap();
        follows.follow(u2, artist.id).await.unwrap();
        devices.add(u1, "good");
        devices.add(u2, "dead");
        push.set_unregistered("dead");

        // Call the fan-out directly (deterministic — avoids the bg spawn).
        let data = vec![("kind".to_string(), "new_release".to_string())];
        svc.push_fanout(&[u1, u2], "New release", "The Chase", &data)
            .await;

        // Both tokens were attempted.
        let mut sent = push.sent();
        sent.sort();
        assert_eq!(sent, vec!["dead".to_string(), "good".to_string()]);
        // The unregistered one was pruned; the good one kept.
        assert_eq!(devices.tokens_for(u1), vec!["good"]);
        assert!(devices.tokens_for(u2).is_empty());
    }
}
