//! `ServerClient` — the single surface the rest of the app talks to.
//!
//! gRPC primary, REST fallback. On a transport-level failure (channel
//! refused, codec, timeout — anything that smells like "the gRPC layer
//! itself isn't working"), we fall back to REST for the same call. We do
//! NOT fall back on auth errors (`Unauthenticated`/`Forbidden`) — those are
//! the server speaking, and the answer is the same on both transports.

use serde::{Deserialize, Serialize};

use super::grpc::GrpcClient;
use super::rest::RestClient;
use super::{
    Album, Artist, ChunkAck, Credential, DiscoverSection, EpisodeProgress, FingerprintStatus,
    LibraryStorage, ListeningStats, MetadataEdit, NotificationPage, PermissionTier, PlayHistoryPage,
    PlayInput,
    Playlist, PlaylistWithTracks, Podcast, PodcastCandidate, PodcastEpisode, RefreshReport,
    RescanReport, ServerConfig, Track, UploadEvent, UploadInitRequest, UploadListFilter,
    UploadResult, UploadSummary, UploadView,
};
use crate::error::{AppError, AppResult};

/// Aggregate client. Cheap to clone-by-`Arc` once placed in Tauri state.
pub struct ServerClient {
    config: ServerConfig,
    rest: RestClient,
}

impl ServerClient {
    pub fn new(config: ServerConfig) -> AppResult<Self> {
        let rest = RestClient::new(&config)?;
        Ok(Self { config, rest })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Username/password login. Tries gRPC first; falls back to REST on
    /// any non-auth transport failure.
    pub async fn login(&self, username: &str, password: &str) -> AppResult<LoginOutcome> {
        let grpc_attempt = try_grpc(&self.config).await;
        if let Ok(client) = &grpc_attempt {
            match client.login(username, password).await {
                Ok(o) => {
                    return Ok(LoginOutcome {
                        token: o.token,
                        user_id: o.user_id,
                        tier: o.tier,
                        expires_at: o.expires_at,
                        transport: TransportUsed::Grpc,
                    });
                }
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC login unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        } else if let Err(e) = &grpc_attempt {
            tracing::info!(err = %e, "gRPC connect failed; falling back to REST");
        }
        let o = self.rest.login(username, password).await?;
        Ok(LoginOutcome {
            token: o.token,
            user_id: o.user_id,
            tier: o.tier,
            expires_at: o.expires_at,
            transport: TransportUsed::Rest,
        })
    }

    /// Resolve a credential to a server identity (and therefore tier).
    pub async fn whoami(&self, cred: &Credential) -> AppResult<WhoAmI> {
        if let Ok(client) = try_grpc(&self.config).await {
            match client.whoami(cred).await {
                Ok(w) => {
                    return Ok(WhoAmI {
                        kind: w.kind,
                        user_id: w.user_id,
                        username: w.username,
                        tier: w.tier,
                        transport: TransportUsed::Grpc,
                    });
                }
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC whoami unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        }
        let w = self.rest.whoami(cred).await?;
        Ok(WhoAmI {
            kind: w.kind,
            user_id: w.user_id,
            username: w.username,
            tier: w.tier,
            transport: TransportUsed::Rest,
        })
    }

    /// Revoke a session.
    pub async fn logout(&self, cred: &Credential) -> AppResult<()> {
        if let Ok(client) = try_grpc(&self.config).await {
            match client.logout(cred).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => {
                    tracing::info!(err = %e, "gRPC logout unavailable; falling back to REST");
                }
                Err(e) => return Err(e),
            }
        }
        self.rest.logout(cred).await
    }

    /// Register a new account. Server-gated to Admin callers (or
    /// `SECRET_KEY`); the active credential is attached for authorization.
    /// Returns the new user id. Auth/merit rejections (403 / invalid /
    /// duplicate) do NOT trigger the REST fallback — the server spoke.
    pub async fn register(
        &self,
        cred: &Credential,
        username: &str,
        password: &str,
        level: super::PermissionTier,
    ) -> AppResult<String> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.register(cred, username, password, level).await {
                Ok(id) => return Ok(id),
                Err(e) if is_transport_error(&e) => fallback_log("register", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.register(cred, username, password, level).await
    }

    /// Change (or admin-reset) a user's password. `old_password` empty for
    /// admin/secret-key resets; required + verified server-side for non-admin
    /// self-changes. Auth/merit rejections (403 / unauthenticated / invalid /
    /// not-found) do NOT trigger the REST fallback — the server spoke.
    pub async fn change_password(
        &self,
        cred: &Credential,
        target_user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .change_password(cred, target_user_id, old_password, new_password)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("change_password", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest
            .change_password(cred, target_user_id, old_password, new_password)
            .await
    }

    /// List every registered user. Admin-gated; returns id/username/tier.
    /// Used by the client to populate the admin password-reset dropdown.
    pub async fn list_users(&self, cred: &Credential) -> AppResult<Vec<super::UserEntry>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_users(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_users", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_users(cred).await
    }

    /// Delete a user account. Admin-gated; reuses `map_password_err` for the
    /// gRPC error mapping (same auth/merit semantics — `PermissionDenied`→
    /// `Forbidden`, `NotFound`→`Internal`, transport faults fall back).
    pub async fn delete_user(&self, cred: &Credential, user_id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_user(cred, user_id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_user", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_user(cred, user_id).await
    }

    /// Liveness probe used by the online/offline detector. We only use REST
    /// `/health` here — the server exposes it cheaply and tonic-health
    /// would require a separate proto wiring for marginal benefit.
    pub async fn health(&self) -> AppResult<bool> {
        self.rest.health().await
    }

    /// Probe **both** transports so the UI can show which are actually working,
    /// not just a single online bit. REST is the cheap `/health` GET; gRPC is a
    /// real unary call ([`GrpcClient::probe`]) rather than a bare connect —
    /// otherwise a reverse proxy / LB that accepts the connection makes gRPC
    /// read "online" while its backend is down (and the app then thinks the
    /// server is reachable). Both probes run concurrently.
    pub async fn transport_health(&self) -> TransportHealth {
        let (rest, grpc) = tokio::join!(self.rest.health(), GrpcClient::probe(&self.config));
        TransportHealth {
            rest: rest.unwrap_or(false),
            grpc,
        }
    }

    // ----- Library reads -------------------------------------------------

    pub async fn list_artists(
        &self,
        cred: &Credential,
        limit: i64,
        offset: i64,
    ) -> AppResult<(Vec<Artist>, i64)> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_artists(cred, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_artists(cred, limit, offset).await
    }

    pub async fn search_artists(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Artist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_artists(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_artists(cred, query, limit, offset).await
    }

    pub async fn list_albums_by_artist(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_albums_by_artist(cred, artist_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_albums_by_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_albums_by_artist(cred, artist_id).await
    }

    pub async fn search_albums(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_albums(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_albums", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_albums(cred, query, limit, offset).await
    }

    pub async fn list_tracks_by_album(
        &self,
        cred: &Credential,
        album_id: &str,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_tracks_by_album(cred, album_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_tracks_by_album", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_tracks_by_album(cred, album_id).await
    }

    pub async fn search_tracks(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_tracks(cred, query, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_tracks", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_tracks(cred, query, limit, offset).await
    }

    // ----- Get-by-id (sync reconcile) ------------------------------------

    pub async fn get_artist(&self, cred: &Credential, id: &str) -> AppResult<Option<Artist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_artist(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_artist(cred, id).await
    }

    pub async fn get_album(&self, cred: &Credential, id: &str) -> AppResult<Option<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_album(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_album", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_album(cred, id).await
    }

    pub async fn get_track(&self, cred: &Credential, id: &str) -> AppResult<Option<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_track(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_track(cred, id).await
    }

    pub async fn get_library_storage(&self, cred: &Credential) -> AppResult<LibraryStorage> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_library_storage(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_library_storage", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_library_storage(cred).await
    }

    // ----- Delete (Manager+ gated server-side) ----------------------------

    pub async fn delete_artist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_artist(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_artist(cred, id).await
    }

    pub async fn delete_album(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_album(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_album", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_album(cred, id).await
    }

    pub async fn delete_track(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_track(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_track(cred, id).await
    }

    // ----- Metadata edit (Phase 9; Manager+ gated server-side) -------------

    pub async fn edit_track_metadata(
        &self,
        cred: &Credential,
        id: &str,
        edit: &MetadataEdit,
    ) -> AppResult<Track> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.edit_track_metadata(cred, id, edit).await {
                Ok(t) => return Ok(t),
                Err(e) if is_transport_error(&e) => fallback_log("edit_track_metadata", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.edit_track_metadata(cred, id, edit).await
    }

    // ----- Merge + aliases (Phase 10; Manager+ gated server-side) ----------

    pub async fn merge_artists(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Artist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.merge_artists(cred, survivor_id, duplicate_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("merge_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.merge_artists(cred, survivor_id, duplicate_id).await
    }

    pub async fn merge_albums(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Album> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.merge_albums(cred, survivor_id, duplicate_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("merge_albums", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.merge_albums(cred, survivor_id, duplicate_id).await
    }

    pub async fn move_track(
        &self,
        cred: &Credential,
        track_id: &str,
        album_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.move_track(cred, track_id, album_id, single_release).await {
                Ok(t) => return Ok(t),
                Err(e) if is_transport_error(&e) => fallback_log("move_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.move_track(cred, track_id, album_id, single_release).await
    }

    pub async fn set_track_single_release(
        &self,
        cred: &Credential,
        track_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.set_track_single_release(cred, track_id, single_release).await {
                Ok(t) => return Ok(t),
                Err(e) if is_transport_error(&e) => fallback_log("set_track_single_release", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.set_track_single_release(cred, track_id, single_release).await
    }

    pub async fn add_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> AppResult<Artist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.add_artist_alias(cred, artist_id, name, sort_name, language).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("add_artist_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.add_artist_alias(cred, artist_id, name, sort_name, language).await
    }

    pub async fn remove_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.remove_artist_alias(cred, artist_id, alias_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("remove_artist_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.remove_artist_alias(cred, artist_id, alias_id).await
    }

    pub async fn set_primary_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.set_primary_artist_alias(cred, artist_id, alias_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("set_primary_artist_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.set_primary_artist_alias(cred, artist_id, alias_id).await
    }

    pub async fn add_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<Album> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.add_album_alias(cred, album_id, title, language).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("add_album_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.add_album_alias(cred, album_id, title, language).await
    }

    pub async fn remove_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.remove_album_alias(cred, album_id, alias_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("remove_album_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.remove_album_alias(cred, album_id, alias_id).await
    }

    pub async fn set_primary_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.set_primary_album_alias(cred, album_id, alias_id).await {
                Ok(a) => return Ok(a),
                Err(e) if is_transport_error(&e) => fallback_log("set_primary_album_alias", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.set_primary_album_alias(cred, album_id, alias_id).await
    }

    // ----- Follows & notifications (Phase 10) ------------------------------

    pub async fn follow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.follow_artist(cred, artist_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("follow_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.follow_artist(cred, artist_id).await
    }

    pub async fn unfollow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.unfollow_artist(cred, artist_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("unfollow_artist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.unfollow_artist(cred, artist_id).await
    }

    pub async fn is_following(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.is_following(cred, artist_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("is_following", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.is_following(cred, artist_id).await
    }

    pub async fn list_following(&self, cred: &Credential) -> AppResult<Vec<Artist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_following(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_following", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_following(cred).await
    }

    pub async fn list_notifications(
        &self,
        cred: &Credential,
        unread_only: bool,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<NotificationPage> {
        if let Some(grpc) = self.try_grpc().await {
            // gRPC takes i32 (proto); 0 = "server default" for limit.
            let l = limit.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            let o = offset.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            match grpc.list_notifications(cred, unread_only, l, o).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_notifications", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_notifications(cred, unread_only, limit, offset).await
    }

    pub async fn notifications_unread_count(&self, cred: &Credential) -> AppResult<i64> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.notifications_unread_count(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("notifications_unread_count", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.notifications_unread_count(cred).await
    }

    pub async fn mark_notification_read(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.mark_notification_read(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("mark_notification_read", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.mark_notification_read(cred, id).await
    }

    pub async fn mark_all_notifications_read(&self, cred: &Credential) -> AppResult<u64> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.mark_all_notifications_read(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("mark_all_notifications_read", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.mark_all_notifications_read(cred).await
    }

    pub async fn register_device(
        &self,
        cred: &Credential,
        token: &str,
        platform: &str,
    ) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.register_device(cred, token, platform).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("register_device", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.register_device(cred, token, platform).await
    }

    pub async fn unregister_device(&self, cred: &Credential, token: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.unregister_device(cred, token).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("unregister_device", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.unregister_device(cred, token).await
    }

    // ----- Play history (Phase 11) -----------------------------------------

    pub async fn record_plays(&self, cred: &Credential, events: &[PlayInput]) -> AppResult<u64> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.record_plays(cred, events).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("record_plays", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.record_plays(cred, events).await
    }

    pub async fn list_play_history(
        &self,
        cred: &Credential,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<PlayHistoryPage> {
        if let Some(grpc) = self.try_grpc().await {
            let l = limit.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            let o = offset.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            match grpc.list_play_history(cred, l, o).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_play_history", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_play_history(cred, limit, offset).await
    }

    pub async fn play_stats(
        &self,
        cred: &Credential,
        window_days: Option<i64>,
        limit: Option<i64>,
    ) -> AppResult<ListeningStats> {
        if let Some(grpc) = self.try_grpc().await {
            let w = window_days.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            let l = limit.unwrap_or(0).clamp(0, i32::MAX as i64) as i32;
            match grpc.play_stats(cred, w, l).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("play_stats", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.play_stats(cred, window_days, limit).await
    }

    // ----- Favorites (Phase 11) --------------------------------------------

    pub async fn favorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.favorite(cred, kind, entity_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("favorite", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.favorite(cred, kind, entity_id).await
    }

    pub async fn unfavorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.unfavorite(cred, kind, entity_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("unfavorite", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.unfavorite(cred, kind, entity_id).await
    }

    pub async fn is_favorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.is_favorite(cred, kind, entity_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("is_favorite", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.is_favorite(cred, kind, entity_id).await
    }

    pub async fn list_favorite_tracks(&self, cred: &Credential) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_favorite_tracks(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_favorite_tracks", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_favorite_tracks(cred).await
    }

    pub async fn list_favorite_albums(&self, cred: &Credential) -> AppResult<Vec<Album>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_favorite_albums(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_favorite_albums", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_favorite_albums(cred).await
    }

    pub async fn list_favorite_artists(&self, cred: &Credential) -> AppResult<Vec<Artist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_favorite_artists(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_favorite_artists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_favorite_artists(cred).await
    }

    pub async fn favorited_track_ids(&self, cred: &Credential) -> AppResult<Vec<String>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.favorited_track_ids(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("favorited_track_ids", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.favorited_track_ids(cred).await
    }

    // ----- Discover (Phase 11) ---------------------------------------------

    pub async fn discover_home(&self, cred: &Credential) -> AppResult<Vec<DiscoverSection>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.discover_home(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("discover_home", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.discover_home(cred).await
    }

    pub async fn discover_radio(
        &self,
        cred: &Credential,
        seed_artist_id: Option<&str>,
        seed_album_id: Option<&str>,
        seed_track_id: Option<&str>,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .discover_radio(cred, seed_artist_id, seed_album_id, seed_track_id)
                .await
            {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("discover_radio", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest
            .discover_radio(cred, seed_artist_id, seed_album_id, seed_track_id)
            .await
    }

    pub async fn discover_similar(
        &self,
        cred: &Credential,
        track_id: &str,
        limit: i32,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.discover_similar(cred, track_id, limit).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("discover_similar", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.discover_similar(cred, track_id, limit).await
    }

    pub async fn discover_playlist_recommendations(
        &self,
        cred: &Credential,
        seed_track_ids: &[String],
        limit: i32,
    ) -> AppResult<Vec<Track>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .discover_playlist_recommendations(cred, seed_track_ids, limit)
                .await
            {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => {
                    fallback_log("discover_playlist_recommendations", &e)
                }
                Err(e) => return Err(e),
            }
        }
        self.rest
            .discover_playlist_recommendations(cred, seed_track_ids, limit)
            .await
    }

    pub async fn fingerprint_status(&self, cred: &Credential) -> AppResult<FingerprintStatus> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.fingerprint_status(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("fingerprint_status", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.fingerprint_status(cred).await
    }

    // ----- Podcasts --------------------------------------------------------

    pub async fn search_podcasts(
        &self,
        cred: &Credential,
        term: &str,
        limit: i32,
    ) -> AppResult<Vec<PodcastCandidate>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.search_podcasts(cred, term, limit).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("search_podcasts", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.search_podcasts(cred, term, limit).await
    }

    pub async fn subscribe_feed(
        &self,
        cred: &Credential,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> AppResult<Podcast> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.subscribe_feed(cred, feed_url, itunes_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("subscribe_feed", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.subscribe_feed(cred, feed_url, itunes_id).await
    }

    pub async fn list_podcasts(
        &self,
        cred: &Credential,
        limit: i32,
        offset: i32,
    ) -> AppResult<(Vec<Podcast>, i64)> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_podcasts(cred, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_podcasts", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_podcasts(cred, limit, offset).await
    }

    pub async fn get_podcast(&self, cred: &Credential, id: &str) -> AppResult<Podcast> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_podcast(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_podcast", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_podcast(cred, id).await
    }

    pub async fn delete_podcast(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_podcast(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_podcast", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_podcast(cred, id).await
    }

    pub async fn refresh_podcast(&self, cred: &Credential, id: &str) -> AppResult<RefreshReport> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.refresh_podcast(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("refresh_podcast", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.refresh_podcast(cred, id).await
    }

    pub async fn set_podcast_auto_download(
        &self,
        cred: &Credential,
        id: &str,
        auto_download: i32,
    ) -> AppResult<Podcast> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.set_podcast_auto_download(cred, id, auto_download).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("set_auto_download", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.set_podcast_auto_download(cred, id, auto_download).await
    }

    pub async fn list_episodes(
        &self,
        cred: &Credential,
        podcast_id: &str,
        limit: i32,
        offset: i32,
    ) -> AppResult<Vec<PodcastEpisode>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_episodes(cred, podcast_id, limit, offset).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_episodes", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_episodes(cred, podcast_id, limit, offset).await
    }

    pub async fn get_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_episode(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_episode", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_episode(cred, id).await
    }

    pub async fn download_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.download_episode(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("download_episode", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.download_episode(cred, id).await
    }

    pub async fn subscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.subscribe_podcast(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("subscribe", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.subscribe_podcast(cred, id).await
    }

    pub async fn unsubscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.unsubscribe_podcast(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("unsubscribe", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.unsubscribe_podcast(cred, id).await
    }

    pub async fn is_subscribed(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.is_subscribed(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("is_subscribed", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.is_subscribed(cred, id).await
    }

    pub async fn list_subscriptions(&self, cred: &Credential) -> AppResult<Vec<Podcast>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_subscriptions(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_subscriptions", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_subscriptions(cred).await
    }

    pub async fn record_episode_progress(
        &self,
        cred: &Credential,
        episode_id: &str,
        position_ms: i64,
        completed: bool,
    ) -> AppResult<EpisodeProgress> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .record_episode_progress(cred, episode_id, position_ms, completed)
                .await
            {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("record_episode_progress", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest
            .record_episode_progress(cred, episode_id, position_ms, completed)
            .await
    }

    pub async fn list_episode_progress(
        &self,
        cred: &Credential,
        podcast_id: &str,
    ) -> AppResult<Vec<EpisodeProgress>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_episode_progress(cred, podcast_id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_episode_progress", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_episode_progress(cred, podcast_id).await
    }

    // ----- Image upload (Phase 9) ------------------------------------------
    //
    // REST-only: binary blob upload, mirroring the REST-only cover *serving*
    // (`GET /albums/:id/cover`). No gRPC path, so no fallback dance.

    pub async fn upload_album_cover(
        &self,
        cred: &Credential,
        album_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        self.rest.upload_album_cover(cred, album_id, bytes, content_type).await
    }

    pub async fn upload_artist_image(
        &self,
        cred: &Credential,
        artist_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        self.rest.upload_artist_image(cred, artist_id, bytes, content_type).await
    }

    // ----- Playlists (sync pull + push) ----------------------------------

    pub async fn list_my_playlists(&self, cred: &Credential) -> AppResult<Vec<Playlist>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_my_playlists(cred).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("list_my_playlists", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_my_playlists(cred).await
    }

    pub async fn get_playlist(
        &self,
        cred: &Credential,
        id: &str,
    ) -> AppResult<Option<PlaylistWithTracks>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_playlist(cred, id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("get_playlist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_playlist(cred, id).await
    }

    pub async fn create_playlist(&self, cred: &Credential, name: &str) -> AppResult<Playlist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.create_playlist(cred, name).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("create_playlist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.create_playlist(cred, name).await
    }

    pub async fn rename_playlist(
        &self,
        cred: &Credential,
        id: &str,
        name: &str,
    ) -> AppResult<Playlist> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.rename_playlist(cred, id, name).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transport_error(&e) => fallback_log("rename_playlist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.rename_playlist(cred, id, name).await
    }

    pub async fn delete_playlist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.delete_playlist(cred, id).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("delete_playlist", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.delete_playlist(cred, id).await
    }

    pub async fn add_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.add_playlist_track(cred, playlist_id, track_id, position).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("add_playlist_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.add_playlist_track(cred, playlist_id, track_id, position).await
    }

    pub async fn remove_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        position: i32,
    ) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.remove_playlist_track(cred, playlist_id, position).await {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("remove_playlist_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.remove_playlist_track(cred, playlist_id, position).await
    }

    pub async fn reorder_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<()> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .reorder_playlist_track(cred, playlist_id, from_position, to_position)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if is_transport_error(&e) => fallback_log("reorder_playlist_track", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest
            .reorder_playlist_track(cred, playlist_id, from_position, to_position)
            .await
    }

    // ----- Uploads (Phase 8) -----------------------------------------------

    /// Upload a file (single audio or archive). Manager+ gated server-side.
    /// Tries gRPC (client-streaming) first; falls back to REST multipart.
    /// Auth/merit rejections do NOT trigger the fallback — the server spoke.
    pub async fn upload_file(
        &self,
        cred: &Credential,
        filename: String,
        data: Vec<u8>,
        cover: Option<(String, Vec<u8>)>,
    ) -> AppResult<UploadResult> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .upload_file(cred, filename.clone(), data.clone(), cover.clone())
                .await
            {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("upload_file", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.upload_file(cred, filename, data, cover).await
    }

    // ----- Uploads v2 (gRPC primary, REST + WS fallback) ------------------

    pub async fn init_upload(
        &self,
        cred: &Credential,
        body: &UploadInitRequest,
    ) -> AppResult<UploadView> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.init_upload(cred, body).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("init_upload", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.init_upload(cred, body).await
    }

    pub async fn put_chunk(
        &self,
        cred: &Credential,
        upload_id: &str,
        file_index: u32,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> AppResult<ChunkAck> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc
                .put_chunk(cred, upload_id, file_index, chunk_index, data.clone())
                .await
            {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("put_chunk", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest
            .put_chunk(cred, upload_id, file_index, chunk_index, data)
            .await
    }

    pub async fn get_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.get_upload(cred, id).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("get_upload", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.get_upload(cred, id).await
    }

    pub async fn list_uploads(
        &self,
        cred: &Credential,
        filter: &UploadListFilter,
    ) -> AppResult<Vec<UploadSummary>> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.list_uploads(cred, filter).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("list_uploads", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.list_uploads(cred, filter).await
    }

    pub async fn cancel_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.cancel_upload(cred, id).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("cancel_upload", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.cancel_upload(cred, id).await
    }

    pub async fn pause_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.pause_upload(cred, id).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("pause_upload", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.pause_upload(cred, id).await
    }

    pub async fn resume_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.resume_upload(cred, id).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("resume_upload", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.resume_upload(cred, id).await
    }

    /// Subscribe to the live `uploads` channel — gRPC server-stream primary,
    /// WebSocket fallback. Returns a receiver the caller drains for events.
    pub async fn subscribe_uploads(
        &self,
        cred: &Credential,
    ) -> AppResult<tokio::sync::mpsc::Receiver<UploadEvent>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<UploadEvent>(128);
        if let Some(grpc) = self.try_grpc().await {
            match grpc.stream_uploads(cred).await {
                Ok(stream) => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        use futures_util::StreamExt;
                        futures_util::pin_mut!(stream);
                        while let Some(ev) = stream.next().await {
                            if tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    });
                    return Ok(rx);
                }
                Err(e) if is_transport_error(&e) => fallback_log("stream_uploads", &e),
                Err(e) => return Err(e),
            }
        }
        // WS fallback spawns its own reader into `tx`.
        self.rest.stream_uploads(cred, tx).await?;
        Ok(rx)
    }

    // ----- Rescan library (Phase 8+) ---------------------------------------

    /// Re-measure durations for all tracks. Manager+ gated server-side.
    pub async fn rescan_library(&self, cred: &Credential) -> AppResult<RescanReport> {
        if let Some(grpc) = self.try_grpc().await {
            match grpc.rescan_library(cred).await {
                Ok(r) => return Ok(r),
                Err(e) if is_transport_error(&e) => fallback_log("rescan_library", &e),
                Err(e) => return Err(e),
            }
        }
        self.rest.rescan_library(cred).await
    }

    /// Open a gRPC channel for one logical operation. Returns `None` (not
    /// `Err`) when the channel can't be opened so the call sites can just
    /// fall through to REST without nested matches.
    async fn try_grpc(&self) -> Option<GrpcClient> {
        match GrpcClient::connect(&self.config).await {
            Ok(c) => Some(c),
            Err(e) => {
                fallback_log("connect", &e);
                None
            }
        }
    }
}

fn fallback_log(op: &str, err: &AppError) {
    tracing::info!(op, err = %err, "gRPC unavailable; falling back to REST");
}

async fn try_grpc(config: &ServerConfig) -> AppResult<GrpcClient> {
    GrpcClient::connect(config).await
}

fn is_transport_error(err: &AppError) -> bool {
    matches!(err, AppError::Transport(_))
}

/// Which transport actually serviced a successful call. Surfaced to the UI
/// for diagnostics; not part of the auth contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportUsed {
    Grpc,
    Rest,
}

/// Reachability of each server transport. Surfaced to the UI so the user can
/// see whether gRPC (primary) and REST (fallback) are each working — not just
/// a single online/offline bit. The app is [`online`](Self::online) when
/// *either* transport is up, since calls fall back gRPC → REST automatically.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TransportHealth {
    pub rest: bool,
    pub grpc: bool,
}

impl TransportHealth {
    /// Can we reach the server at all? True when either transport is up.
    pub fn online(&self) -> bool {
        self.rest || self.grpc
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginOutcome {
    pub token: String,
    pub user_id: String,
    pub tier: PermissionTier,
    pub expires_at: String,
    pub transport: TransportUsed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoAmI {
    pub kind: String,
    pub user_id: String,
    pub username: String,
    pub tier: PermissionTier,
    pub transport: TransportUsed,
}
