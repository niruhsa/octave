//! In-memory active-session state, persisted through a `SecureStore`.
//!
//! Holds the current `Credential` (if any), the most recent `WhoAmI`
//! snapshot, and a debounced online/offline flag. Commands read this via
//! Tauri's managed state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::store::{SecureStore, StoredCredential, StoredCredentialKind};
use crate::error::{AppError, AppResult};
use crate::transport::{
    Album, Artist, ChunkAck, Credential, DiscoverSection, EpisodeProgress, FingerprintStatus,
    ListeningStats, MetadataEdit, NotificationPage, PermissionTier, PlayHistoryPage, PlayInput,
    Podcast,
    PodcastCandidate, PodcastEpisode, RefreshReport, RescanReport, ServerClient, ServerConfig,
    Track, TransportHealth, TransportUsed, UploadEvent, UploadInitRequest, UploadListFilter,
    UploadResult, UploadSummary, UploadView,
};

/// A snapshot of the active session safe to hand to the frontend. Mirrors
/// `StoredCredential` minus the secret material.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub kind: StoredCredentialKind,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub tier: PermissionTier,
    pub expires_at: Option<String>,
}

pub struct AuthManager {
    server: Arc<ServerClient>,
    store: Arc<dyn SecureStore>,
    state: RwLock<Option<StoredCredential>>,
    /// Last-known reachability of each transport (gRPC primary, REST fallback).
    health: RwLock<TransportHealth>,
}

impl AuthManager {
    pub fn new(server: Arc<ServerClient>, store: Arc<dyn SecureStore>) -> Self {
        Self {
            server,
            store,
            state: RwLock::new(None),
            health: RwLock::new(TransportHealth::default()),
        }
    }

    /// Read the persisted credential (if any) into memory. Called once at
    /// startup. Errors during read are logged and treated as "no session" —
    /// a corrupt keychain entry shouldn't brick the app.
    pub async fn restore_from_store(&self) {
        match self.store.load().await {
            Ok(Some(cred)) => {
                tracing::info!(kind = ?cred.kind, "restored auth credential from secure store");
                *self.state.write().await = Some(cred);
            }
            Ok(None) => tracing::debug!("no stored credential"),
            Err(e) => tracing::warn!(err = %e, "failed to restore credential; continuing anonymous"),
        }
    }

    pub async fn current(&self) -> Option<AuthSession> {
        self.state.read().await.as_ref().map(|c| AuthSession {
            kind: c.kind,
            user_id: c.user_id.clone(),
            username: c.username.clone(),
            tier: c.tier.unwrap_or(PermissionTier::User),
            expires_at: c.expires_at.clone(),
        })
    }

    /// Return the active `Credential` for an outbound call, or
    /// `AuthNotConfigured` if nobody has logged in / set a key.
    pub async fn credential(&self) -> AppResult<Credential> {
        let guard = self.state.read().await;
        let cred = guard
            .as_ref()
            .ok_or_else(|| AppError::AuthNotConfigured("no active session".into()))?;
        Ok(match cred.kind {
            StoredCredentialKind::SecretKey => Credential::SecretKey(cred.secret.clone()),
            StoredCredentialKind::Bearer => Credential::Bearer(cred.secret.clone()),
        })
    }

    pub async fn is_online(&self) -> bool {
        self.health.read().await.online()
    }

    /// Probe both transports and update the cached health. Returns whether the
    /// server is reachable on *either* transport (the online/offline bit).
    pub async fn refresh_online(&self) -> bool {
        self.refresh_transports().await.online()
    }

    /// Last-known per-transport reachability (cached snapshot; no network).
    pub async fn transport_health(&self) -> TransportHealth {
        *self.health.read().await
    }

    /// Probe gRPC + REST concurrently and cache the result. Drives the
    /// per-transport status indicator.
    pub async fn refresh_transports(&self) -> TransportHealth {
        let health = self.server.transport_health().await;
        *self.health.write().await = health;
        health
    }

    /// Username/password login. Persists the resulting bearer token AND
    /// the server URLs so the app can auto-reconnect on restart (no
    /// re-typing the server address). Only Bearer sessions are
    /// auto-restored — `SECRET_KEY` entries carry URLs too but are
    /// skipped by the boot-time restore.
    pub async fn login(&self, username: &str, password: &str) -> AppResult<AuthSession> {
        let outcome = self.server.login(username, password).await?;
        let cfg = self.server.config();
        let cred = StoredCredential {
            kind: StoredCredentialKind::Bearer,
            secret: outcome.token.clone(),
            rest_url: Some(cfg.rest_url.clone()),
            grpc_url: Some(cfg.grpc_url.clone()),
            grpc_explicit: Some(cfg.grpc_explicit),
            user_id: Some(outcome.user_id.clone()),
            username: Some(username.to_string()),
            tier: Some(outcome.tier),
            expires_at: Some(outcome.expires_at.clone()),
        };
        self.store.save(&cred).await?;
        *self.state.write().await = Some(cred);
        // Seed health from the transport that just succeeded; the periodic
        // probe corrects the other transport within a second.
        *self.health.write().await = seed_health(outcome.transport);
        Ok(AuthSession {
            kind: StoredCredentialKind::Bearer,
            user_id: Some(outcome.user_id),
            username: Some(username.to_string()),
            tier: outcome.tier,
            expires_at: Some(outcome.expires_at),
        })
    }

    /// Install a pre-shared `SECRET_KEY` as the active credential. We
    /// verify it via `WhoAmI` before persisting so the user sees an
    /// immediate failure for typos instead of a silent broken state.
    /// Server URLs are saved too, but SecretKey sessions are NOT
    /// auto-restored on boot (the user must re-enter the key).
    pub async fn set_secret_key(&self, key: &str) -> AppResult<AuthSession> {
        let probe = Credential::SecretKey(key.to_string());
        let who = self.server.whoami(&probe).await?;
        let cfg = self.server.config();
        let cred = StoredCredential {
            kind: StoredCredentialKind::SecretKey,
            secret: key.to_string(),
            rest_url: Some(cfg.rest_url.clone()),
            grpc_url: Some(cfg.grpc_url.clone()),
            grpc_explicit: Some(cfg.grpc_explicit),
            user_id: None,
            username: None,
            tier: Some(who.tier),
            expires_at: None,
        };
        self.store.save(&cred).await?;
        *self.state.write().await = Some(cred);
        *self.health.write().await = seed_health(who.transport);
        Ok(AuthSession {
            kind: StoredCredentialKind::SecretKey,
            user_id: None,
            username: None,
            tier: who.tier,
            expires_at: None,
        })
    }

    /// Register a new account on the server. Server-gated to Admin callers
    /// (or `SECRET_KEY`, which is effective Admin). The active credential
    /// authorizes the call; the new account is NOT logged in locally —
    /// the admin stays signed in. Returns the new user id.
    pub async fn register(
        &self,
        username: &str,
        password: &str,
        tier: PermissionTier,
    ) -> AppResult<String> {
        let cred = self.credential().await?;
        self.server.register(&cred, username, password, tier).await
    }

    /// Change (or admin-reset) a user's password. The active credential
    /// authorizes the call. `old_password` is empty for admin/secret-key
    /// resets; required + verified server-side for non-admin self-changes.
    /// The caller picks the target `user_id` (self-change uses the
    /// session's own id). The session is NOT invalidated — the user keeps
    /// their current token; a re-login with the new password works too.
    pub async fn change_password(
        &self,
        target_user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server
            .change_password(&cred, target_user_id, old_password, new_password)
            .await
    }

    /// List every registered user (admin-gated server-side). Returns
    /// id/username/tier for each — no password hashes.
    pub async fn list_users(&self) -> AppResult<Vec<crate::transport::UserEntry>> {
        let cred = self.credential().await?;
        self.server.list_users(&cred).await
    }

    /// Delete a user account (admin-gated server-side). The active
    /// credential authorizes the call. `target_user_id` is the UUID of
    /// the user to delete.
    pub async fn delete_user(&self, target_user_id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.delete_user(&cred, target_user_id).await
    }

    /// Upload a file (single audio or archive) to the server. Manager+
    /// gated server-side. `source_path` is an absolute path to a local
    /// file — the command handler reads it before calling this method.
    pub async fn upload_file(
        &self,
        filename: String,
        data: Vec<u8>,
        cover: Option<(String, Vec<u8>)>,
    ) -> AppResult<UploadResult> {
        let cred = self.credential().await?;
        self.server
            .upload_file(&cred, filename, data, cover)
            .await
    }

    // ----- Uploads v2 (sessions + reports + live stream) ------------------

    pub async fn init_upload(&self, req: UploadInitRequest) -> AppResult<UploadView> {
        let cred = self.credential().await?;
        self.server.init_upload(&cred, &req).await
    }

    pub async fn put_chunk(
        &self,
        upload_id: String,
        file_index: u32,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> AppResult<ChunkAck> {
        let cred = self.credential().await?;
        self.server
            .put_chunk(&cred, &upload_id, file_index, chunk_index, data)
            .await
    }

    pub async fn get_upload(&self, id: String) -> AppResult<UploadView> {
        let cred = self.credential().await?;
        self.server.get_upload(&cred, &id).await
    }

    pub async fn list_uploads(&self, filter: UploadListFilter) -> AppResult<Vec<UploadSummary>> {
        let cred = self.credential().await?;
        self.server.list_uploads(&cred, &filter).await
    }

    pub async fn cancel_upload(&self, id: String) -> AppResult<UploadView> {
        let cred = self.credential().await?;
        self.server.cancel_upload(&cred, &id).await
    }

    pub async fn pause_upload(&self, id: String) -> AppResult<UploadView> {
        let cred = self.credential().await?;
        self.server.pause_upload(&cred, &id).await
    }

    pub async fn resume_upload(&self, id: String) -> AppResult<UploadView> {
        let cred = self.credential().await?;
        self.server.resume_upload(&cred, &id).await
    }

    pub async fn subscribe_uploads(&self) -> AppResult<tokio::sync::mpsc::Receiver<UploadEvent>> {
        let cred = self.credential().await?;
        self.server.subscribe_uploads(&cred).await
    }

    /// Delete an artist, album, or track. Manager+ gated server-side.
    pub async fn delete_artist(&self, id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.delete_artist(&cred, id).await
    }

    pub async fn delete_album(&self, id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.delete_album(&cred, id).await
    }

    pub async fn delete_track(&self, id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.delete_track(&cred, id).await
    }

    /// Re-measure durations for every track in the library. Manager+ gated.
    pub async fn rescan_library(&self) -> AppResult<RescanReport> {
        let cred = self.credential().await?;
        self.server.rescan_library(&cred).await
    }

    /// Apply an opt-in metadata edit to a track. Manager+ gated server-side.
    pub async fn edit_track_metadata(
        &self,
        id: &str,
        edit: &MetadataEdit,
    ) -> AppResult<Track> {
        let cred = self.credential().await?;
        self.server.edit_track_metadata(&cred, id, edit).await
    }

    // ----- Follows & notifications (Phase 10) ------------------------------

    pub async fn follow_artist(&self, artist_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.follow_artist(&cred, artist_id).await
    }

    pub async fn unfollow_artist(&self, artist_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.unfollow_artist(&cred, artist_id).await
    }

    pub async fn is_following(&self, artist_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.is_following(&cred, artist_id).await
    }

    pub async fn list_following(&self) -> AppResult<Vec<Artist>> {
        let cred = self.credential().await?;
        self.server.list_following(&cred).await
    }

    pub async fn list_notifications(
        &self,
        unread_only: bool,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<NotificationPage> {
        let cred = self.credential().await?;
        self.server.list_notifications(&cred, unread_only, limit, offset).await
    }

    pub async fn notifications_unread_count(&self) -> AppResult<i64> {
        let cred = self.credential().await?;
        self.server.notifications_unread_count(&cred).await
    }

    pub async fn mark_notification_read(&self, id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.mark_notification_read(&cred, id).await
    }

    pub async fn mark_all_notifications_read(&self) -> AppResult<u64> {
        let cred = self.credential().await?;
        self.server.mark_all_notifications_read(&cred).await
    }

    pub async fn register_device(&self, token: &str, platform: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.register_device(&cred, token, platform).await
    }

    // ----- Play history (Phase 11) -----------------------------------------

    pub async fn record_plays(&self, events: &[PlayInput]) -> AppResult<u64> {
        let cred = self.credential().await?;
        self.server.record_plays(&cred, events).await
    }

    pub async fn list_play_history(
        &self,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<PlayHistoryPage> {
        let cred = self.credential().await?;
        self.server.list_play_history(&cred, limit, offset).await
    }

    pub async fn play_stats(
        &self,
        window_days: Option<i64>,
        limit: Option<i64>,
    ) -> AppResult<ListeningStats> {
        let cred = self.credential().await?;
        self.server.play_stats(&cred, window_days, limit).await
    }

    // ----- Favorites (Phase 11) --------------------------------------------

    pub async fn favorite(&self, kind: &str, entity_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.favorite(&cred, kind, entity_id).await
    }

    pub async fn unfavorite(&self, kind: &str, entity_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.unfavorite(&cred, kind, entity_id).await
    }

    pub async fn is_favorite(&self, kind: &str, entity_id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.is_favorite(&cred, kind, entity_id).await
    }

    pub async fn list_favorite_tracks(&self) -> AppResult<Vec<Track>> {
        let cred = self.credential().await?;
        self.server.list_favorite_tracks(&cred).await
    }

    pub async fn list_favorite_albums(&self) -> AppResult<Vec<Album>> {
        let cred = self.credential().await?;
        self.server.list_favorite_albums(&cred).await
    }

    pub async fn list_favorite_artists(&self) -> AppResult<Vec<Artist>> {
        let cred = self.credential().await?;
        self.server.list_favorite_artists(&cred).await
    }

    pub async fn favorited_track_ids(&self) -> AppResult<Vec<String>> {
        let cred = self.credential().await?;
        self.server.favorited_track_ids(&cred).await
    }

    // ----- Discover (Phase 11) ---------------------------------------------

    pub async fn discover_home(&self) -> AppResult<Vec<DiscoverSection>> {
        let cred = self.credential().await?;
        self.server.discover_home(&cred).await
    }

    pub async fn discover_radio(
        &self,
        seed_artist_id: Option<&str>,
        seed_album_id: Option<&str>,
        seed_track_id: Option<&str>,
    ) -> AppResult<Vec<Track>> {
        let cred = self.credential().await?;
        self.server
            .discover_radio(&cred, seed_artist_id, seed_album_id, seed_track_id)
            .await
    }

    /// Acoustic "sounds like this" — the seed track's nearest neighbors (Phase 12).
    pub async fn discover_similar(&self, track_id: &str, limit: i32) -> AppResult<Vec<Track>> {
        let cred = self.credential().await?;
        self.server.discover_similar(&cred, track_id, limit).await
    }

    /// Spotify-style playlist recommendations (Phase 12). `seed_track_ids` is the
    /// playlist's current songs; results are based on + exclude them.
    pub async fn discover_playlist_recommendations(
        &self,
        seed_track_ids: &[String],
        limit: i32,
    ) -> AppResult<Vec<Track>> {
        let cred = self.credential().await?;
        self.server
            .discover_playlist_recommendations(&cred, seed_track_ids, limit)
            .await
    }

    /// Fingerprint analysis coverage (Phase 12).
    pub async fn fingerprint_status(&self) -> AppResult<FingerprintStatus> {
        let cred = self.credential().await?;
        self.server.fingerprint_status(&cred).await
    }

    pub async fn unregister_device(&self, token: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.unregister_device(&cred, token).await
    }

    // ----- Podcasts --------------------------------------------------------

    pub async fn search_podcasts(
        &self,
        term: &str,
        limit: i32,
    ) -> AppResult<Vec<PodcastCandidate>> {
        let cred = self.credential().await?;
        self.server.search_podcasts(&cred, term, limit).await
    }

    pub async fn subscribe_feed(
        &self,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> AppResult<Podcast> {
        let cred = self.credential().await?;
        self.server.subscribe_feed(&cred, feed_url, itunes_id).await
    }

    pub async fn list_podcasts(&self, limit: i32, offset: i32) -> AppResult<(Vec<Podcast>, i64)> {
        let cred = self.credential().await?;
        self.server.list_podcasts(&cred, limit, offset).await
    }

    pub async fn get_podcast(&self, id: &str) -> AppResult<Podcast> {
        let cred = self.credential().await?;
        self.server.get_podcast(&cred, id).await
    }

    pub async fn delete_podcast(&self, id: &str) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.delete_podcast(&cred, id).await
    }

    pub async fn refresh_podcast(&self, id: &str) -> AppResult<RefreshReport> {
        let cred = self.credential().await?;
        self.server.refresh_podcast(&cred, id).await
    }

    pub async fn set_podcast_auto_download(
        &self,
        id: &str,
        auto_download: i32,
    ) -> AppResult<Podcast> {
        let cred = self.credential().await?;
        self.server.set_podcast_auto_download(&cred, id, auto_download).await
    }

    pub async fn list_episodes(
        &self,
        podcast_id: &str,
        limit: i32,
        offset: i32,
    ) -> AppResult<Vec<PodcastEpisode>> {
        let cred = self.credential().await?;
        self.server.list_episodes(&cred, podcast_id, limit, offset).await
    }

    pub async fn get_episode(&self, id: &str) -> AppResult<PodcastEpisode> {
        let cred = self.credential().await?;
        self.server.get_episode(&cred, id).await
    }

    pub async fn download_episode_server(&self, id: &str) -> AppResult<PodcastEpisode> {
        let cred = self.credential().await?;
        self.server.download_episode(&cred, id).await
    }

    pub async fn subscribe_podcast(&self, id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.subscribe_podcast(&cred, id).await
    }

    pub async fn unsubscribe_podcast(&self, id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.unsubscribe_podcast(&cred, id).await
    }

    pub async fn is_subscribed(&self, id: &str) -> AppResult<bool> {
        let cred = self.credential().await?;
        self.server.is_subscribed(&cred, id).await
    }

    pub async fn list_subscriptions(&self) -> AppResult<Vec<Podcast>> {
        let cred = self.credential().await?;
        self.server.list_subscriptions(&cred).await
    }

    pub async fn record_episode_progress(
        &self,
        episode_id: &str,
        position_ms: i64,
        completed: bool,
    ) -> AppResult<EpisodeProgress> {
        let cred = self.credential().await?;
        self.server
            .record_episode_progress(&cred, episode_id, position_ms, completed)
            .await
    }

    pub async fn list_episode_progress(&self, podcast_id: &str) -> AppResult<Vec<EpisodeProgress>> {
        let cred = self.credential().await?;
        self.server.list_episode_progress(&cred, podcast_id).await
    }

    // ----- Merge + aliases (Phase 10; Manager+ gated server-side) ----------

    pub async fn merge_artists(&self, survivor_id: &str, duplicate_id: &str) -> AppResult<Artist> {
        let cred = self.credential().await?;
        self.server.merge_artists(&cred, survivor_id, duplicate_id).await
    }

    pub async fn merge_albums(&self, survivor_id: &str, duplicate_id: &str) -> AppResult<Album> {
        let cred = self.credential().await?;
        self.server.merge_albums(&cred, survivor_id, duplicate_id).await
    }

    pub async fn move_track(
        &self,
        track_id: &str,
        album_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let cred = self.credential().await?;
        self.server.move_track(&cred, track_id, album_id, single_release).await
    }

    pub async fn set_track_single_release(
        &self,
        track_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let cred = self.credential().await?;
        self.server.set_track_single_release(&cred, track_id, single_release).await
    }

    pub async fn add_artist_alias(
        &self,
        artist_id: &str,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> AppResult<Artist> {
        let cred = self.credential().await?;
        self.server.add_artist_alias(&cred, artist_id, name, sort_name, language).await
    }

    pub async fn remove_artist_alias(&self, artist_id: &str, alias_id: &str) -> AppResult<Artist> {
        let cred = self.credential().await?;
        self.server.remove_artist_alias(&cred, artist_id, alias_id).await
    }

    pub async fn set_primary_artist_alias(
        &self,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        let cred = self.credential().await?;
        self.server.set_primary_artist_alias(&cred, artist_id, alias_id).await
    }

    pub async fn add_album_alias(
        &self,
        album_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<Album> {
        let cred = self.credential().await?;
        self.server.add_album_alias(&cred, album_id, title, language).await
    }

    pub async fn remove_album_alias(&self, album_id: &str, alias_id: &str) -> AppResult<Album> {
        let cred = self.credential().await?;
        self.server.remove_album_alias(&cred, album_id, alias_id).await
    }

    pub async fn set_primary_album_alias(
        &self,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        let cred = self.credential().await?;
        self.server.set_primary_album_alias(&cred, album_id, alias_id).await
    }

    /// Upload a cover image for an album. Manager+ gated server-side.
    pub async fn upload_album_cover(
        &self,
        album_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.upload_album_cover(&cred, album_id, bytes, content_type).await
    }

    /// Upload an image for an artist. Manager+ gated server-side.
    pub async fn upload_artist_image(
        &self,
        artist_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        let cred = self.credential().await?;
        self.server.upload_artist_image(&cred, artist_id, bytes, content_type).await
    }

    /// Resolve the current credential against the server. Updates the
    /// cached tier so the UI reflects server-side changes (e.g. an admin
    /// downgraded the user) on next refresh.
    pub async fn whoami(&self) -> AppResult<AuthSession> {
        let cred = self.credential().await?;
        let who = self.server.whoami(&cred).await?;
        let mut guard = self.state.write().await;
        if let Some(c) = guard.as_mut() {
            c.tier = Some(who.tier);
            if !who.user_id.is_empty() {
                c.user_id = Some(who.user_id.clone());
            }
            if !who.username.is_empty() {
                c.username = Some(who.username.clone());
            }
            self.store.save(c).await?;
        }
        Ok(AuthSession {
            kind: guard.as_ref().map(|c| c.kind).unwrap_or(StoredCredentialKind::Bearer),
            user_id: if who.user_id.is_empty() { None } else { Some(who.user_id) },
            username: if who.username.is_empty() { None } else { Some(who.username) },
            tier: who.tier,
            expires_at: guard.as_ref().and_then(|c| c.expires_at.clone()),
        })
    }

    /// Re-point an already-authenticated session at a (possibly different)
    /// server: validate the stored credential against the new server and
    /// persist the new URLs so a restart reconnects there.
    ///
    /// - `Ok(Some(session))` — the credential is accepted (same server at a new
    ///   address, or a different server that recognises the token); the new
    ///   URLs are saved and the session kept.
    /// - `Ok(None)` — the credential was rejected (401/403) on the new server;
    ///   it's wiped locally so the UI can route to login.
    /// - new server unreachable — the credential + new URLs are kept
    ///   optimistically so it reconnects once the server is back.
    ///
    /// Assumes `self` is a freshly built manager whose `ServerClient` already
    /// points at the new URLs and whose state was loaded via
    /// [`restore_from_store`](Self::restore_from_store).
    pub async fn revalidate_for_new_server(&self) -> AppResult<Option<AuthSession>> {
        let existing = { self.state.read().await.clone() };
        let Some(mut cred) = existing else {
            // Nothing was logged in — just record reachability.
            let _ = self.refresh_online().await;
            return Ok(None);
        };
        let cfg = self.server.config();
        cred.rest_url = Some(cfg.rest_url.clone());
        cred.grpc_url = Some(cfg.grpc_url.clone());
        cred.grpc_explicit = Some(cfg.grpc_explicit);
        let probe = match cred.kind {
            StoredCredentialKind::SecretKey => Credential::SecretKey(cred.secret.clone()),
            StoredCredentialKind::Bearer => Credential::Bearer(cred.secret.clone()),
        };
        match self.server.whoami(&probe).await {
            Ok(who) => {
                cred.tier = Some(who.tier);
                if !who.user_id.is_empty() {
                    cred.user_id = Some(who.user_id.clone());
                }
                if !who.username.is_empty() {
                    cred.username = Some(who.username.clone());
                }
                self.store.save(&cred).await?;
                *self.health.write().await = seed_health(who.transport);
                let session = AuthSession {
                    kind: cred.kind,
                    user_id: cred.user_id.clone(),
                    username: cred.username.clone(),
                    tier: who.tier,
                    expires_at: cred.expires_at.clone(),
                };
                *self.state.write().await = Some(cred);
                Ok(Some(session))
            }
            // The server answered, but our credential isn't valid there.
            Err(AppError::Unauthenticated(_)) | Err(AppError::Forbidden(_)) => {
                self.store.clear().await.ok();
                *self.state.write().await = None;
                // Server is reachable (it rejected us) — probe both transports
                // so the login screen reflects the real state.
                let _ = self.refresh_transports().await;
                Ok(None)
            }
            // New server unreachable — keep the credential + new URLs so it
            // reconnects once the server is back.
            Err(_) => {
                self.store.save(&cred).await?;
                *self.health.write().await = TransportHealth::default();
                let session = AuthSession {
                    kind: cred.kind,
                    user_id: cred.user_id.clone(),
                    username: cred.username.clone(),
                    tier: cred.tier.unwrap_or(PermissionTier::User),
                    expires_at: cred.expires_at.clone(),
                };
                *self.state.write().await = Some(cred);
                Ok(Some(session))
            }
        }
    }

    /// Log out: best-effort server revocation, then wipe local state.
    pub async fn logout(&self) -> AppResult<()> {
        if let Ok(cred) = self.credential().await {
            if let Err(e) = self.server.logout(&cred).await {
                tracing::warn!(err = %e, "server logout failed; clearing local state anyway");
            }
        }
        self.store.clear().await?;
        *self.state.write().await = None;
        Ok(())
    }

    pub fn server(&self) -> &ServerClient {
        &self.server
    }

    pub fn server_config(&self) -> ServerConfig {
        self.server.config().clone()
    }
}

/// Seed transport health from the transport that just serviced an auth call,
/// so the indicator is right immediately without blocking the call on a fresh
/// gRPC handshake (which can stall on `connect_timeout` when gRPC is down):
/// - via gRPC → gRPC is up; REST is assumed up (same host; corrected by the
///   ~1 s background probe if not).
/// - via REST → REST is up; gRPC is down (REST is only reached as a fallback
///   *after* the gRPC attempt fails).
fn seed_health(used: TransportUsed) -> TransportHealth {
    match used {
        TransportUsed::Grpc => TransportHealth { rest: true, grpc: true },
        TransportUsed::Rest => TransportHealth { rest: true, grpc: false },
    }
}
