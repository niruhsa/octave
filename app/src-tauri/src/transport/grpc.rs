//! gRPC client over the server's auth service.
//!
//! Connection is lazy and per-call: `Endpoint::connect()` is cheap once the
//! channel is warm (tonic reuses HTTP/2 streams), and constructing a new
//! channel on failure is simpler than keeping state for reconnect logic at
//! this stage. Phase 5 may add a long-lived channel + reconnect strategy.

use std::time::Duration;

use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::Request;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

use super::proto::auth::auth_service_client::AuthServiceClient;
use super::proto::auth::{LoginRequest, LogoutRequest, WhoAmIRequest};
use super::proto::auth::{ChangePasswordRequest, DeleteUserRequest, ListUsersRequest, RegisterRequest, RegisterResponse};
use super::proto::library::library_service_client::LibraryServiceClient;
use super::proto::library::{
    AddAlbumAliasRequest, AddArtistAliasRequest, DeleteAlbumRequest, DeleteArtistRequest,
    DeleteTrackRequest, EditTrackMetadataRequest, GetAlbumRequest, GetArtistRequest,
    GetTrackRequest, ListAlbumsByArtistRequest, ListArtistsRequest, ListTracksByAlbumRequest,
    MergeRequest, MoveTrackRequest, Pagination, RemoveAliasRequest, SearchRequest,
    SetPrimaryAliasRequest, SetSingleReleaseRequest,
};
use super::proto::playlist::playlist_service_client::PlaylistServiceClient;
use super::proto::playlist::{
    AddPlaylistTrackRequest, CreatePlaylistRequest, DeletePlaylistRequest, GetPlaylistRequest,
    ListMyPlaylistsRequest, RemovePlaylistTrackRequest, RenamePlaylistRequest,
    ReorderPlaylistTrackRequest,
};
use super::proto::upload::upload_service_client::UploadServiceClient;
use super::proto::upload::{UploadInfo, UploadRequest, UploadResponse as PbUploadResponse};
use super::proto::upload as pb;
use super::proto::notification::notification_service_client::NotificationServiceClient;
use super::proto::notification as npb;
use super::proto::podcast::podcast_service_client::PodcastServiceClient;
use super::proto::podcast as ppb;
use super::{
    Album, ArchiveUploadResult, Artist, ChunkAck, Credential, MetadataEdit, Notification,
    NotificationPage, PermissionTier, Playlist, PlaylistTrack, PlaylistWithTracks, Podcast,
    PodcastCandidate, PodcastEpisode, RefreshReport, RescanReport, ServerConfig,
    SingleUploadResult, Track, UploadEvent, UploadFileInit, UploadFileView, UploadInitRequest,
    UploadListFilter, UploadResult, UploadSummary, UploadView,
};
use crate::error::{AppError, AppResult};

/// Thin wrapper around the generated `AuthServiceClient`.
pub struct GrpcClient {
    channel: Channel,
}

impl GrpcClient {
    /// Open a gRPC channel to the configured server. Returns an error if
    /// the endpoint URL can't be parsed or the TCP/HTTP2 handshake fails
    /// within the connect timeout.
    pub async fn connect(config: &ServerConfig) -> AppResult<Self> {
        let endpoint = Self::endpoint(config.grpc_endpoint())?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .tcp_nodelay(true);

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| AppError::Transport(format!("gRPC connect: {e}")))?;
        Ok(Self { channel })
    }

    /// Build a tonic [`Endpoint`] for the gRPC URL, attaching a TLS config
    /// when the scheme is `https`.
    ///
    /// tonic only speaks TLS when a [`ClientTlsConfig`] is present — an
    /// `https` URL is **not** enough on its own. The auto-TLS shortcut lives
    /// in `Endpoint::new` (what the generated `…Client::connect` uses), but we
    /// build the channel by hand via `Endpoint::from_shared`, which goes
    /// through `From<Uri>` and leaves TLS off. Without this, an `https`
    /// endpoint fails the connect with a bare "transport error" (tonic's
    /// `HttpsUriWithoutTlsSupport`) before any handshake — exactly the symptom
    /// when the server has `GRPC_TLS` on while the client still tries h2c.
    ///
    /// We attach the system trust roots (`with_enabled_roots`, matching
    /// tonic's own codegen default); the URL host drives cert verification and
    /// SNI. A cert that doesn't chain to a root in the OS trust store (e.g. a
    /// bare self-signed cert) still needs an explicit CA via `ca_certificate`.
    fn endpoint(url: &str) -> AppResult<Endpoint> {
        let endpoint = Endpoint::from_shared(url.to_string())
            .map_err(|e| AppError::Transport(format!("invalid gRPC endpoint: {e}")))?;
        if url.starts_with("https://") {
            return endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|e| AppError::Transport(format!("gRPC TLS config: {e}")));
        }
        Ok(endpoint)
    }

    /// gRPC liveness probe for the connectivity UI / online flag.
    ///
    /// Uses the standard gRPC health-checking protocol
    /// (`grpc.health.v1.Health/Check` on the overall `""` service, which the
    /// server marks `SERVING`). gRPC is "up" **only** when the Check returns
    /// `SERVING` — i.e. the real service answered. A bare connect (TCP/HTTP2
    /// handshake) isn't enough: a reverse proxy / load balancer accepts the
    /// connection even when its backend is down, which is what made gRPC
    /// falsely read online while REST's `/health` correctly failed. Any
    /// error — connect refused/timeout, `Unimplemented`, a non-gRPC responder,
    /// or a non-`SERVING` status — counts as down.
    ///
    /// Short + hard-capped: this runs ~once a second, so connect and the Check
    /// each get a tight budget and the whole probe is wrapped in an outer 4 s
    /// timeout so a hung/black-holed connection can't keep it alive.
    pub async fn probe(config: &ServerConfig) -> bool {
        let fut = async {
            let endpoint = match Self::endpoint(config.grpc_endpoint()) {
                Ok(e) => e
                    .connect_timeout(Duration::from_secs(3))
                    .timeout(Duration::from_secs(3))
                    .tcp_nodelay(true),
                Err(_) => return false,
            };
            let channel = match endpoint.connect().await {
                Ok(c) => c,
                Err(_) => return false,
            };
            // `""` = overall server health per the gRPC health spec.
            let req = HealthCheckRequest { service: String::new() };
            match HealthClient::new(channel).check(req).await {
                Ok(resp) => resp.into_inner().status == ServingStatus::Serving as i32,
                Err(_) => false,
            }
        };
        tokio::time::timeout(Duration::from_secs(4), fut)
            .await
            .unwrap_or(false)
    }

    fn client(&self) -> AuthServiceClient<Channel> {
        AuthServiceClient::new(self.channel.clone())
    }

    fn library(&self) -> LibraryServiceClient<Channel> {
        LibraryServiceClient::new(self.channel.clone())
    }

    fn playlists(&self) -> PlaylistServiceClient<Channel> {
        PlaylistServiceClient::new(self.channel.clone())
    }

    fn uploads(&self) -> UploadServiceClient<Channel> {
        UploadServiceClient::new(self.channel.clone())
    }

    fn notifications(&self) -> NotificationServiceClient<Channel> {
        NotificationServiceClient::new(self.channel.clone())
    }

    fn podcasts(&self) -> PodcastServiceClient<Channel> {
        PodcastServiceClient::new(self.channel.clone())
    }

    /// Username/password login. On success the server returns an opaque
    /// session token; the caller is responsible for storing it via the
    /// secure store.
    pub async fn login(&self, username: &str, password: &str) -> AppResult<GrpcLoginOutcome> {
        let req = Request::new(LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        });
        let resp = self
            .client()
            .login(req)
            .await
            .map_err(|s| AppError::Transport(format!("login: {s}")))?
            .into_inner();
        Ok(GrpcLoginOutcome {
            token: resp.token,
            user_id: resp.user_id,
            tier: PermissionTier::from_proto(resp.level),
            expires_at: resp.expires_at,
        })
    }

    /// Resolve the current credential to its server identity.
    pub async fn whoami(&self, cred: &Credential) -> AppResult<GrpcWhoAmI> {
        let mut req = Request::new(WhoAmIRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .client()
            .who_am_i(req)
            .await
            .map_err(|s| AppError::Transport(format!("whoami: {s}")))?
            .into_inner();
        Ok(GrpcWhoAmI {
            kind: resp.kind,
            user_id: resp.user_id,
            username: resp.username,
            tier: PermissionTier::from_proto(resp.level),
        })
    }

    /// Revoke the current bearer token server-side. No-op for `SecretKey`
    /// credentials (the server returns OK).
    pub async fn logout(&self, cred: &Credential) -> AppResult<()> {
        let mut req = Request::new(LogoutRequest {});
        attach_credential(&mut req, cred)?;
        self.client()
            .logout(req)
            .await
            .map_err(|s| AppError::Transport(format!("logout: {s}")))?;
        Ok(())
    }

    /// Register a new account. Server-gated to Admin callers (or
    /// `SECRET_KEY`, which is effective Admin); the caller's credential is
    /// attached so the server can authorize. Returns the new user id.
    pub async fn register(
        &self,
        cred: &Credential,
        username: &str,
        password: &str,
        level: super::PermissionTier,
    ) -> AppResult<String> {
        let mut req = Request::new(RegisterRequest {
            username: username.to_string(),
            password: password.to_string(),
            level: tier_to_proto_level(level),
        });
        attach_credential(&mut req, cred)?;
        let resp: RegisterResponse = self
            .client()
            .register(req)
            .await
            .map_err(map_register_err)?
            .into_inner();
        Ok(resp.user_id)
    }

    /// Change (or admin-reset) a user's password. `old_password` is empty
    /// for admin/secret-key resets; required + verified server-side for
    /// non-admin self-changes. The caller's credential authorizes the call.
    pub async fn change_password(
        &self,
        cred: &Credential,
        target_user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> AppResult<()> {
        let mut req = Request::new(ChangePasswordRequest {
            user_id: target_user_id.to_string(),
            old_password: old_password.to_string(),
            new_password: new_password.to_string(),
        });
        attach_credential(&mut req, cred)?;
        self.client()
            .change_password(req)
            .await
            .map_err(map_password_err)?;
        Ok(())
    }

    /// List every registered user. Admin-gated server-side; returns each
    /// user's id, username, and tier — no password hashes.
    pub async fn list_users(&self, cred: &Credential) -> AppResult<Vec<super::UserEntry>> {
        let mut req = Request::new(ListUsersRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .client()
            .list_users(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_users: {s}")))?
            .into_inner();
        Ok(resp
            .users
            .into_iter()
            .map(|u| super::UserEntry {
                id: u.id,
                username: u.username,
                level: super::PermissionTier::from_proto(u.level),
            })
            .collect())
    }

    /// Delete a user account. Admin-gated server-side. The caller's
    /// credential is attached for authorization.
    pub async fn delete_user(&self, cred: &Credential, user_id: &str) -> AppResult<()> {
        let mut req = Request::new(DeleteUserRequest {
            user_id: user_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        self.client()
            .delete_user(req)
            .await
            .map_err(map_password_err)?;
        Ok(())
    }

    // ----- Library reads -------------------------------------------------

    pub async fn list_artists(
        &self,
        cred: &Credential,
        limit: i64,
        offset: i64,
    ) -> AppResult<(Vec<Artist>, i64)> {
        let mut req = Request::new(ListArtistsRequest {
            page: Some(Pagination { limit, offset }),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .list_artists(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_artists: {s}")))?
            .into_inner();
        Ok((resp.artists.into_iter().map(artist_from_proto).collect(), resp.total))
    }

    pub async fn search_artists(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Artist>> {
        let mut req = Request::new(SearchRequest {
            query: query.to_string(),
            page: Some(Pagination { limit, offset }),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .search_artists(req)
            .await
            .map_err(|s| AppError::Transport(format!("search_artists: {s}")))?
            .into_inner();
        Ok(resp.artists.into_iter().map(artist_from_proto).collect())
    }

    pub async fn list_albums_by_artist(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<Album>> {
        let mut req = Request::new(ListAlbumsByArtistRequest {
            artist_id: artist_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .list_albums_by_artist(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_albums_by_artist: {s}")))?
            .into_inner();
        Ok(resp.albums.into_iter().map(album_from_proto).collect())
    }

    pub async fn search_albums(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Album>> {
        let mut req = Request::new(SearchRequest {
            query: query.to_string(),
            page: Some(Pagination { limit, offset }),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .search_albums(req)
            .await
            .map_err(|s| AppError::Transport(format!("search_albums: {s}")))?
            .into_inner();
        Ok(resp.albums.into_iter().map(album_from_proto).collect())
    }

    pub async fn list_tracks_by_album(
        &self,
        cred: &Credential,
        album_id: &str,
    ) -> AppResult<Vec<Track>> {
        let mut req = Request::new(ListTracksByAlbumRequest {
            album_id: album_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .list_tracks_by_album(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_tracks_by_album: {s}")))?
            .into_inner();
        Ok(resp.tracks.into_iter().map(track_from_proto).collect())
    }

    pub async fn search_tracks(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Track>> {
        let mut req = Request::new(SearchRequest {
            query: query.to_string(),
            page: Some(Pagination { limit, offset }),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .search_tracks(req)
            .await
            .map_err(|s| AppError::Transport(format!("search_tracks: {s}")))?
            .into_inner();
        Ok(resp.tracks.into_iter().map(track_from_proto).collect())
    }

    // ----- Get-by-id (sync reconcile) ------------------------------------
    //
    // `NotFound` maps to `Ok(None)` so the sync engine can treat a missing
    // server row as "prune locally" without special-casing the gRPC status.

    pub async fn get_artist(&self, cred: &Credential, id: &str) -> AppResult<Option<Artist>> {
        let mut req = Request::new(GetArtistRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        match self.library().get_artist(req).await {
            Ok(r) => Ok(Some(artist_from_proto(r.into_inner()))),
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(AppError::Transport(format!("get_artist: {s}"))),
        }
    }

    pub async fn get_album(&self, cred: &Credential, id: &str) -> AppResult<Option<Album>> {
        let mut req = Request::new(GetAlbumRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        match self.library().get_album(req).await {
            Ok(r) => Ok(Some(album_from_proto(r.into_inner()))),
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(AppError::Transport(format!("get_album: {s}"))),
        }
    }

    pub async fn get_track(&self, cred: &Credential, id: &str) -> AppResult<Option<Track>> {
        let mut req = Request::new(GetTrackRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        match self.library().get_track(req).await {
            Ok(r) => Ok(Some(track_from_proto(r.into_inner()))),
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(AppError::Transport(format!("get_track: {s}"))),
        }
    }

    // ----- Delete (Manager+ gated server-side) ----------------------------

    pub async fn delete_artist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(DeleteArtistRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.library()
            .delete_artist(req)
            .await
            .map_err(map_mutation_err("delete_artist"))?;
        Ok(())
    }

    pub async fn delete_album(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(DeleteAlbumRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.library()
            .delete_album(req)
            .await
            .map_err(map_mutation_err("delete_album"))?;
        Ok(())
    }

    pub async fn delete_track(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(DeleteTrackRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.library()
            .delete_track(req)
            .await
            .map_err(map_mutation_err("delete_track"))?;
        Ok(())
    }

    // ----- Metadata edit (Phase 9; Manager+ gated server-side) -------------

    /// Apply an opt-in metadata edit to a track. Auth/merit rejections map to
    /// permanent errors (no REST fallback); only transport faults fall back.
    pub async fn edit_track_metadata(
        &self,
        cred: &Credential,
        id: &str,
        edit: &MetadataEdit,
    ) -> AppResult<Track> {
        let mut req = Request::new(EditTrackMetadataRequest {
            id: id.to_string(),
            title: edit.title.clone(),
            track_no: edit.track_no,
            disc_no: edit.disc_no,
            metadata_json: edit.metadata_json.clone(),
            year: edit.year,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .edit_track_metadata(req)
            .await
            .map_err(map_mutation_err("edit_track_metadata"))?
            .into_inner();
        Ok(track_from_proto(resp))
    }

    // ----- Merge + aliases (Phase 10; Manager+ gated server-side) ----------

    pub async fn merge_artists(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Artist> {
        let mut req = Request::new(MergeRequest {
            survivor_id: survivor_id.to_string(),
            duplicate_id: duplicate_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .merge_artists(req)
            .await
            .map_err(map_mutation_err("merge_artists"))?
            .into_inner();
        Ok(artist_from_proto(resp))
    }

    pub async fn merge_albums(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Album> {
        let mut req = Request::new(MergeRequest {
            survivor_id: survivor_id.to_string(),
            duplicate_id: duplicate_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .merge_albums(req)
            .await
            .map_err(map_mutation_err("merge_albums"))?
            .into_inner();
        Ok(album_from_proto(resp))
    }

    pub async fn move_track(
        &self,
        cred: &Credential,
        track_id: &str,
        album_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let mut req = Request::new(MoveTrackRequest {
            track_id: track_id.to_string(),
            album_id: album_id.to_string(),
            single_release,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .move_track(req)
            .await
            .map_err(map_mutation_err("move_track"))?
            .into_inner();
        Ok(track_from_proto(resp))
    }

    pub async fn set_track_single_release(
        &self,
        cred: &Credential,
        track_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let mut req = Request::new(SetSingleReleaseRequest {
            track_id: track_id.to_string(),
            single_release,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .set_track_single_release(req)
            .await
            .map_err(map_mutation_err("set_track_single_release"))?
            .into_inner();
        Ok(track_from_proto(resp))
    }

    pub async fn add_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> AppResult<Artist> {
        let mut req = Request::new(AddArtistAliasRequest {
            artist_id: artist_id.to_string(),
            name: name.to_string(),
            sort_name: sort_name.unwrap_or_default().to_string(),
            language: language.unwrap_or_default().to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .add_artist_alias(req)
            .await
            .map_err(map_mutation_err("add_artist_alias"))?
            .into_inner();
        Ok(artist_from_proto(resp))
    }

    pub async fn remove_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        let mut req = Request::new(RemoveAliasRequest {
            entity_id: artist_id.to_string(),
            alias_id: alias_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .remove_artist_alias(req)
            .await
            .map_err(map_mutation_err("remove_artist_alias"))?
            .into_inner();
        Ok(artist_from_proto(resp))
    }

    pub async fn set_primary_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        let mut req = Request::new(SetPrimaryAliasRequest {
            entity_id: artist_id.to_string(),
            alias_id: alias_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .set_primary_artist_alias(req)
            .await
            .map_err(map_mutation_err("set_primary_artist_alias"))?
            .into_inner();
        Ok(artist_from_proto(resp))
    }

    pub async fn add_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<Album> {
        let mut req = Request::new(AddAlbumAliasRequest {
            album_id: album_id.to_string(),
            title: title.to_string(),
            language: language.unwrap_or_default().to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .add_album_alias(req)
            .await
            .map_err(map_mutation_err("add_album_alias"))?
            .into_inner();
        Ok(album_from_proto(resp))
    }

    pub async fn remove_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        let mut req = Request::new(RemoveAliasRequest {
            entity_id: album_id.to_string(),
            alias_id: alias_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .remove_album_alias(req)
            .await
            .map_err(map_mutation_err("remove_album_alias"))?
            .into_inner();
        Ok(album_from_proto(resp))
    }

    pub async fn set_primary_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        let mut req = Request::new(SetPrimaryAliasRequest {
            entity_id: album_id.to_string(),
            alias_id: alias_id.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .set_primary_album_alias(req)
            .await
            .map_err(map_mutation_err("set_primary_album_alias"))?
            .into_inner();
        Ok(album_from_proto(resp))
    }

    // ----- Follows & notifications (Phase 10) ----------------------------

    /// Follow an artist. Returns the resulting follow state (`true`). A
    /// `SECRET_KEY` identity is rejected server-side (no user to own the
    /// follow) → a permanent error, no REST fallback.
    pub async fn follow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        let mut req = Request::new(npb::FollowRequest { artist_id: artist_id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .follow_artist(req)
            .await
            .map_err(map_mutation_err("follow_artist"))?
            .into_inner();
        Ok(resp.following)
    }

    /// Unfollow an artist. Returns the resulting follow state (`false`).
    pub async fn unfollow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        let mut req = Request::new(npb::FollowRequest { artist_id: artist_id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .unfollow_artist(req)
            .await
            .map_err(map_mutation_err("unfollow_artist"))?
            .into_inner();
        Ok(resp.following)
    }

    pub async fn is_following(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        let mut req = Request::new(npb::FollowRequest { artist_id: artist_id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .is_following(req)
            .await
            .map_err(|s| AppError::Transport(format!("is_following: {s}")))?
            .into_inner();
        Ok(resp.following)
    }

    /// The artists the caller follows. Mapped onto the public `Artist` type
    /// (no aliases — the follow list is a slim projection).
    pub async fn list_following(&self, cred: &Credential) -> AppResult<Vec<Artist>> {
        let mut req = Request::new(npb::ListFollowingRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .list_following(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_following: {s}")))?
            .into_inner();
        Ok(resp.artists.into_iter().map(followed_artist_from_proto).collect())
    }

    pub async fn list_notifications(
        &self,
        cred: &Credential,
        unread_only: bool,
        limit: i32,
        offset: i32,
    ) -> AppResult<NotificationPage> {
        let mut req = Request::new(npb::ListNotificationsRequest { unread_only, limit, offset });
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .list_notifications(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_notifications: {s}")))?
            .into_inner();
        Ok(NotificationPage {
            notifications: resp.notifications.into_iter().map(notification_from_proto).collect(),
            total: resp.total,
            unread_count: resp.unread_count,
        })
    }

    pub async fn notifications_unread_count(&self, cred: &Credential) -> AppResult<i64> {
        let mut req = Request::new(npb::GetUnreadCountRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .get_unread_count(req)
            .await
            .map_err(|s| AppError::Transport(format!("get_unread_count: {s}")))?
            .into_inner();
        Ok(resp.unread_count)
    }

    pub async fn mark_notification_read(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(npb::MarkReadRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.notifications()
            .mark_notification_read(req)
            .await
            .map_err(map_mutation_err("mark_notification_read"))?;
        Ok(())
    }

    pub async fn mark_all_notifications_read(&self, cred: &Credential) -> AppResult<u64> {
        let mut req = Request::new(npb::MarkAllReadRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .notifications()
            .mark_all_notifications_read(req)
            .await
            .map_err(map_mutation_err("mark_all_notifications_read"))?
            .into_inner();
        Ok(resp.marked.max(0) as u64)
    }

    pub async fn register_device(
        &self,
        cred: &Credential,
        token: &str,
        platform: &str,
    ) -> AppResult<()> {
        let mut req = Request::new(npb::RegisterDeviceRequest {
            token: token.to_string(),
            platform: platform.to_string(),
        });
        attach_credential(&mut req, cred)?;
        self.notifications()
            .register_device(req)
            .await
            .map_err(map_mutation_err("register_device"))?;
        Ok(())
    }

    pub async fn unregister_device(&self, cred: &Credential, token: &str) -> AppResult<()> {
        let mut req = Request::new(npb::UnregisterDeviceRequest {
            token: token.to_string(),
        });
        attach_credential(&mut req, cred)?;
        self.notifications()
            .unregister_device(req)
            .await
            .map_err(map_mutation_err("unregister_device"))?;
        Ok(())
    }

    // ----- Podcasts ------------------------------------------------------

    pub async fn search_podcasts(
        &self,
        cred: &Credential,
        term: &str,
        limit: i32,
    ) -> AppResult<Vec<PodcastCandidate>> {
        let mut req = Request::new(ppb::SearchPodcastsRequest { term: term.to_string(), limit });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .search_podcasts(req)
            .await
            .map_err(|s| AppError::Transport(format!("search_podcasts: {s}")))?
            .into_inner();
        Ok(resp.candidates.into_iter().map(candidate_from_proto).collect())
    }

    pub async fn subscribe_feed(
        &self,
        cred: &Credential,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> AppResult<Podcast> {
        let mut req = Request::new(ppb::SubscribeFeedRequest {
            feed_url: feed_url.unwrap_or_default().to_string(),
            itunes_id: itunes_id.unwrap_or(0),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .subscribe_feed(req)
            .await
            .map_err(map_mutation_err("subscribe_feed"))?
            .into_inner();
        Ok(podcast_from_proto(resp))
    }

    pub async fn list_podcasts(
        &self,
        cred: &Credential,
        limit: i32,
        offset: i32,
    ) -> AppResult<(Vec<Podcast>, i64)> {
        let mut req = Request::new(ppb::ListPodcastsRequest { limit, offset });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .list_podcasts(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_podcasts: {s}")))?
            .into_inner();
        Ok((resp.podcasts.into_iter().map(podcast_from_proto).collect(), resp.total))
    }

    pub async fn get_podcast(&self, cred: &Credential, id: &str) -> AppResult<Podcast> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .get_podcast(req)
            .await
            .map_err(|s| AppError::Transport(format!("get_podcast: {s}")))?
            .into_inner();
        Ok(podcast_from_proto(resp))
    }

    pub async fn delete_podcast(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.podcasts()
            .delete_podcast(req)
            .await
            .map_err(map_mutation_err("delete_podcast"))?;
        Ok(())
    }

    pub async fn refresh_podcast(&self, cred: &Credential, id: &str) -> AppResult<RefreshReport> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .refresh_podcast(req)
            .await
            .map_err(map_mutation_err("refresh_podcast"))?
            .into_inner();
        Ok(RefreshReport {
            podcast_id: resp.podcast_id,
            new_episodes: resp.new_episodes,
            not_modified: resp.not_modified,
        })
    }

    pub async fn set_podcast_auto_download(
        &self,
        cred: &Credential,
        id: &str,
        auto_download: i32,
    ) -> AppResult<Podcast> {
        let mut req = Request::new(ppb::SetAutoDownloadRequest {
            podcast_id: id.to_string(),
            auto_download,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .set_auto_download(req)
            .await
            .map_err(map_mutation_err("set_auto_download"))?
            .into_inner();
        Ok(podcast_from_proto(resp))
    }

    pub async fn list_episodes(
        &self,
        cred: &Credential,
        podcast_id: &str,
        limit: i32,
        offset: i32,
    ) -> AppResult<Vec<PodcastEpisode>> {
        let mut req = Request::new(ppb::ListEpisodesRequest {
            podcast_id: podcast_id.to_string(),
            limit,
            offset,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .list_episodes(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_episodes: {s}")))?
            .into_inner();
        Ok(resp.episodes.into_iter().map(episode_from_proto).collect())
    }

    pub async fn get_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        let mut req = Request::new(ppb::EpisodeIdRequest { episode_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .get_episode(req)
            .await
            .map_err(|s| AppError::Transport(format!("get_episode: {s}")))?
            .into_inner();
        Ok(episode_from_proto(resp))
    }

    /// Trigger the server to download an episode's audio to its disk.
    pub async fn download_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        let mut req = Request::new(ppb::EpisodeIdRequest { episode_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .download_episode(req)
            .await
            .map_err(map_mutation_err("download_episode"))?
            .into_inner();
        Ok(episode_from_proto(resp))
    }

    pub async fn subscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .subscribe(req)
            .await
            .map_err(map_mutation_err("subscribe"))?
            .into_inner();
        Ok(resp.subscribed)
    }

    pub async fn unsubscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .unsubscribe(req)
            .await
            .map_err(map_mutation_err("unsubscribe"))?
            .into_inner();
        Ok(resp.subscribed)
    }

    pub async fn is_subscribed(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        let mut req = Request::new(ppb::PodcastIdRequest { podcast_id: id.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .is_subscribed(req)
            .await
            .map_err(|s| AppError::Transport(format!("is_subscribed: {s}")))?
            .into_inner();
        Ok(resp.subscribed)
    }

    pub async fn list_subscriptions(&self, cred: &Credential) -> AppResult<Vec<Podcast>> {
        let mut req = Request::new(ppb::ListSubscriptionsRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .podcasts()
            .list_subscriptions(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_subscriptions: {s}")))?
            .into_inner();
        Ok(resp.podcasts.into_iter().map(podcast_from_proto).collect())
    }

    // ----- Playlists (sync pull + push) ----------------------------------

    pub async fn list_my_playlists(&self, cred: &Credential) -> AppResult<Vec<Playlist>> {
        let mut req = Request::new(ListMyPlaylistsRequest {});
        attach_credential(&mut req, cred)?;
        let resp = self
            .playlists()
            .list_my_playlists(req)
            .await
            .map_err(|s| AppError::Transport(format!("list_my_playlists: {s}")))?
            .into_inner();
        Ok(resp.playlists.into_iter().map(playlist_from_proto).collect())
    }

    pub async fn get_playlist(
        &self,
        cred: &Credential,
        id: &str,
    ) -> AppResult<Option<PlaylistWithTracks>> {
        let mut req = Request::new(GetPlaylistRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        match self.playlists().get_playlist(req).await {
            Ok(r) => {
                let v = r.into_inner();
                let playlist = v.playlist.map(playlist_from_proto).ok_or_else(|| {
                    AppError::Transport("get_playlist: missing playlist".into())
                })?;
                Ok(Some(PlaylistWithTracks {
                    playlist,
                    tracks: v.tracks.into_iter().map(playlist_track_from_proto).collect(),
                }))
            }
            Err(s) if s.code() == tonic::Code::NotFound => Ok(None),
            Err(s) => Err(AppError::Transport(format!("get_playlist: {s}"))),
        }
    }

    pub async fn create_playlist(&self, cred: &Credential, name: &str) -> AppResult<Playlist> {
        let mut req = Request::new(CreatePlaylistRequest { name: name.to_string() });
        attach_credential(&mut req, cred)?;
        let resp = self
            .playlists()
            .create_playlist(req)
            .await
            .map_err(map_playlist_err("create_playlist"))?
            .into_inner();
        Ok(playlist_from_proto(resp))
    }

    pub async fn rename_playlist(
        &self,
        cred: &Credential,
        id: &str,
        name: &str,
    ) -> AppResult<Playlist> {
        let mut req = Request::new(RenamePlaylistRequest {
            id: id.to_string(),
            name: name.to_string(),
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .playlists()
            .rename_playlist(req)
            .await
            .map_err(map_playlist_err("rename_playlist"))?
            .into_inner();
        Ok(playlist_from_proto(resp))
    }

    pub async fn delete_playlist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let mut req = Request::new(DeletePlaylistRequest { id: id.to_string() });
        attach_credential(&mut req, cred)?;
        self.playlists()
            .delete_playlist(req)
            .await
            .map_err(map_playlist_err("delete_playlist"))?;
        Ok(())
    }

    pub async fn add_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        track_id: &str,
        position: i32,
    ) -> AppResult<()> {
        let mut req = Request::new(AddPlaylistTrackRequest {
            playlist_id: playlist_id.to_string(),
            track_id: track_id.to_string(),
            position,
        });
        attach_credential(&mut req, cred)?;
        self.playlists()
            .add_playlist_track(req)
            .await
            .map_err(map_playlist_err("add_playlist_track"))?;
        Ok(())
    }

    pub async fn remove_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        position: i32,
    ) -> AppResult<()> {
        let mut req = Request::new(RemovePlaylistTrackRequest {
            playlist_id: playlist_id.to_string(),
            position,
        });
        attach_credential(&mut req, cred)?;
        self.playlists()
            .remove_playlist_track(req)
            .await
            .map_err(map_playlist_err("remove_playlist_track"))?;
        Ok(())
    }

    pub async fn reorder_playlist_track(
        &self,
        cred: &Credential,
        playlist_id: &str,
        from_position: i32,
        to_position: i32,
    ) -> AppResult<()> {
        let mut req = Request::new(ReorderPlaylistTrackRequest {
            playlist_id: playlist_id.to_string(),
            from_position,
            to_position,
        });
        attach_credential(&mut req, cred)?;
        self.playlists()
            .reorder_playlist_track(req)
            .await
            .map_err(map_playlist_err("reorder_playlist_track"))?;
        Ok(())
    }

    // ----- Uploads (Phase 8) -----------------------------------------------

    /// Upload a file (single audio or archive) to the server via
    /// client-streaming gRPC. The first message carries an `UploadInfo`
    /// (filename); subsequent messages carry file chunks. Manager+ required.
    pub async fn upload_file(
        &self,
        cred: &Credential,
        filename: String,
        data: Vec<u8>,
        cover: Option<(String, Vec<u8>)>,
    ) -> AppResult<UploadResult> {
        // Build all messages upfront so the stream owns its data
        // (tonic requires 'static).
        let mut msgs: Vec<UploadRequest> = Vec::with_capacity(1 + data.len() / 65536 + 1);
        let (cover_filename, cover_bytes) = match cover {
            Some((name, bytes)) => (name, bytes),
            None => (String::new(), Vec::new()),
        };
        msgs.push(UploadRequest {
            payload: Some(pb::upload_request::Payload::Info(UploadInfo {
                filename,
                cover: cover_bytes,
                cover_filename,
            })),
        });
        for chunk in data.chunks(65536) {
            msgs.push(UploadRequest {
                payload: Some(pb::upload_request::Payload::Chunk(chunk.to_vec())),
            });
        }

        let stream = futures_util::stream::iter(msgs);

        let mut req = Request::new(stream);
        attach_credential(&mut req, cred)?;

        let resp: PbUploadResponse = self
            .uploads()
            .upload(req)
            .await
            .map_err(map_upload_err)?
            .into_inner();

        Ok(if resp.is_archive {
            UploadResult::Archive(ArchiveUploadResult {
                kind: resp.archive_kind,
                ingested: resp.ingested as u64,
                already_indexed: resp.already_indexed as u64,
                non_audio_skipped: resp.non_audio_skipped as u64,
                errors: resp.errors as u64,
                track_ids: resp.track_ids,
            })
        } else {
            UploadResult::Single(SingleUploadResult {
                track_id: resp.single_track_id,
                path: resp.path,
            })
        })
    }

    // ----- Uploads v2 (sessions + reports + live stream) -------------------

    pub async fn init_upload(
        &self,
        cred: &Credential,
        req: &UploadInitRequest,
    ) -> AppResult<UploadView> {
        let files = req.files.iter().map(file_init_to_pb).collect();
        let mut r = Request::new(pb::InitUploadRequest { files });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .init_upload(r)
            .await
            .map_err(map_uploads_err("init_upload"))?
            .into_inner();
        Ok(view_from_pb(resp))
    }

    pub async fn put_chunk(
        &self,
        cred: &Credential,
        upload_id: &str,
        file_index: u32,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> AppResult<ChunkAck> {
        let mut r = Request::new(pb::PutChunkRequest {
            upload_id: upload_id.to_string(),
            file_index,
            chunk_index,
            data,
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .put_chunk(r)
            .await
            .map_err(map_uploads_err("put_chunk"))?
            .into_inner();
        Ok(ack_from_pb(resp))
    }

    pub async fn get_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let mut r = Request::new(pb::GetUploadRequest {
            upload_id: id.to_string(),
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .get_upload(r)
            .await
            .map_err(map_uploads_err("get_upload"))?
            .into_inner();
        Ok(view_from_pb(resp))
    }

    pub async fn list_uploads(
        &self,
        cred: &Credential,
        filter: &UploadListFilter,
    ) -> AppResult<Vec<UploadSummary>> {
        let mut r = Request::new(pb::ListUploadsRequest {
            user_id: filter.user_id.clone().unwrap_or_default(),
            state: filter.state.clone().unwrap_or_default(),
            limit: filter.limit.unwrap_or(0),
            offset: filter.offset.unwrap_or(0),
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .list_uploads(r)
            .await
            .map_err(map_uploads_err("list_uploads"))?
            .into_inner();
        Ok(resp.uploads.into_iter().map(summary_from_pb).collect())
    }

    pub async fn cancel_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let mut r = Request::new(pb::CancelUploadRequest {
            upload_id: id.to_string(),
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .cancel_upload(r)
            .await
            .map_err(map_uploads_err("cancel_upload"))?
            .into_inner();
        Ok(view_from_pb(resp))
    }

    pub async fn pause_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let mut r = Request::new(pb::PauseUploadRequest {
            upload_id: id.to_string(),
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .pause_upload(r)
            .await
            .map_err(map_uploads_err("pause_upload"))?
            .into_inner();
        Ok(view_from_pb(resp))
    }

    pub async fn resume_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let mut r = Request::new(pb::ResumeUploadRequest {
            upload_id: id.to_string(),
        });
        attach_credential(&mut r, cred)?;
        let resp = self
            .uploads()
            .resume_upload(r)
            .await
            .map_err(map_uploads_err("resume_upload"))?
            .into_inner();
        Ok(view_from_pb(resp))
    }

    /// Open the live `uploads` stream (gRPC server-streaming). Yields
    /// `UploadEvent`s the caller is permitted to see; ends on transport error.
    pub async fn stream_uploads(
        &self,
        cred: &Credential,
    ) -> AppResult<impl futures_util::Stream<Item = UploadEvent> + Send + 'static> {
        use futures_util::StreamExt;
        let mut r = Request::new(pb::StreamUploadsRequest {});
        attach_credential(&mut r, cred)?;
        let stream = self
            .uploads()
            .stream_uploads(r)
            .await
            .map_err(map_uploads_err("stream_uploads"))?
            .into_inner();
        Ok(stream.filter_map(|item| async move { item.ok().map(event_from_pb) }))
    }

    // ----- Rescan library (Phase 8+) --------------------------------------

    /// Re-measure actual audio duration for every track in the library.
    /// Updates DB rows that disagree with the measured value. Manager+ gated.
    pub async fn rescan_library(&self, cred: &Credential) -> AppResult<RescanReport> {
        use super::proto::library::RescanRequest;
        let mut req = Request::new(RescanRequest {
            full_metadata: false,
        });
        attach_credential(&mut req, cred)?;
        let resp = self
            .library()
            .rescan_library(req)
            .await
            .map_err(map_mutation_err("rescan_library"))?
            .into_inner();
        Ok(RescanReport {
            tracks_checked: resp.tracks_checked as u64,
            tracks_updated: resp.tracks_updated as u64,
            errors: resp.errors as u64,
        })
    }
}

/// Map a tonic status to the right `AppError` variant for playlist
/// mutations. `PermissionDenied` → `Forbidden` so the sync engine drops
/// the op (server authority); everything else is a transport failure that
/// keeps the op queued for retry.
fn map_playlist_err(op: &'static str) -> impl Fn(tonic::Status) -> AppError {
    move |s| match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("{op}: {s}")),
        tonic::Code::Unauthenticated => AppError::Unauthenticated(format!("{op}: {s}")),
        tonic::Code::NotFound | tonic::Code::InvalidArgument | tonic::Code::FailedPrecondition => {
            // Server rejected the op on its merits — not a transport fault.
            // Surface as Forbidden-class "drop it" by reusing Internal so
            // the engine treats it as a permanent failure.
            AppError::Internal(format!("{op} rejected: {s}"))
        }
        _ => AppError::Transport(format!("{op}: {s}")),
    }
}

/// Map a tonic status from `Register` to the right `AppError`. Same
/// policy as `map_playlist_err`: auth/merit rejections do NOT trigger the
/// REST fallback (the server spoke); only transport-level faults do.
fn map_register_err(s: tonic::Status) -> AppError {
    match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("register: {s}")),
        tonic::Code::Unauthenticated => AppError::Unauthenticated(format!("register: {s}")),
        tonic::Code::InvalidArgument | tonic::Code::AlreadyExists => {
            // Bad username / short password / duplicate — surface the
            // server's message. Internal (not Transport) so the engine
            // doesn't retry / fall back.
            AppError::Internal(format!("register rejected: {s}"))
        }
        _ => AppError::Transport(format!("register: {s}")),
    }
}

// ---- Uploads v2: proto <-> client conversions + error policy ----
// (`opt_str` is shared with the library conversions below.)

fn parse_json(s: String) -> Option<serde_json::Value> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(&s).ok()
    }
}

fn file_init_to_pb(f: &UploadFileInit) -> pb::FileInit {
    pb::FileInit {
        filename: f.filename.clone(),
        hash: f.hash.clone(),
        total_size: f.total_size,
        chunk_size: f.chunk_size,
        total_chunks: f.total_chunks,
        chunks: f
            .chunks
            .iter()
            .map(|c| pb::ChunkInit {
                index: c.index,
                start: c.start,
                end: c.end,
                hash: c.hash.clone(),
            })
            .collect(),
    }
}

fn ack_from_pb(a: pb::ChunkAck) -> ChunkAck {
    ChunkAck {
        file_index: a.file_index,
        chunk_index: a.chunk_index,
        received_chunks: a.received_chunks,
        total_chunks: a.total_chunks,
        file_complete: a.file_complete,
        upload_complete: a.upload_complete,
        state: a.state,
    }
}

fn view_from_pb(v: pb::UploadView) -> UploadView {
    UploadView {
        id: v.id,
        user_id: opt_str(v.user_id),
        state: v.state,
        total_files: v.total_files,
        total_bytes: v.total_bytes,
        bytes_received: v.bytes_received,
        created_at: v.created_at,
        updated_at: v.updated_at,
        error: opt_str(v.error),
        report: parse_json(v.report_json),
        files: v.files.into_iter().map(file_view_from_pb).collect(),
    }
}

fn file_view_from_pb(f: pb::UploadFileView) -> UploadFileView {
    UploadFileView {
        file_index: f.file_index,
        filename: f.filename,
        file_hash: f.file_hash,
        total_size: f.total_size,
        chunk_size: f.chunk_size,
        total_chunks: f.total_chunks,
        received_chunks: f.received_chunks,
        state: f.state,
        error: opt_str(f.error),
        chunks: f
            .chunks
            .into_iter()
            .map(|c| super::UploadChunkView {
                index: c.index,
                start: c.start,
                end: c.end,
                hash: c.hash,
                received: c.received,
            })
            .collect(),
    }
}

fn summary_from_pb(s: pb::UploadSummary) -> UploadSummary {
    UploadSummary {
        id: s.id,
        user_id: opt_str(s.user_id),
        state: s.state,
        total_files: s.total_files,
        total_bytes: s.total_bytes,
        created_at: s.created_at,
        updated_at: s.updated_at,
        error: opt_str(s.error),
    }
}

fn event_from_pb(e: pb::UploadEvent) -> UploadEvent {
    UploadEvent {
        kind: e.kind,
        upload_id: e.upload_id,
        owner_id: opt_str(e.owner_id),
        state: e.state,
        file_index: if e.file_index < 0 {
            None
        } else {
            Some(e.file_index)
        },
        total_files: e.total_files,
        bytes_received: e.bytes_received,
        total_bytes: e.total_bytes,
        chunks_received: e.chunks_received,
        total_chunks: e.total_chunks,
        bytes_per_sec: if e.bytes_per_sec > 0.0 {
            Some(e.bytes_per_sec)
        } else {
            None
        },
        report: parse_json(e.report_json),
    }
}

/// Error policy for uploads v2: auth/merit rejections become non-transport
/// errors (no REST fallback — the server spoke); only transport faults fall back.
fn map_uploads_err(op: &'static str) -> impl Fn(tonic::Status) -> AppError {
    move |s| match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("{op}: {s}")),
        tonic::Code::Unauthenticated => AppError::Unauthenticated(format!("{op}: {s}")),
        tonic::Code::NotFound
        | tonic::Code::InvalidArgument
        | tonic::Code::FailedPrecondition
        | tonic::Code::AlreadyExists => AppError::Internal(format!("{op} rejected: {s}")),
        _ => AppError::Transport(format!("{op}: {s}")),
    }
}

/// Map a tonic status from `UploadFile` to the right `AppError`.
/// Auth/merit rejections do NOT trigger the REST fallback (the server
/// spoke); only transport-level faults do.
fn map_upload_err(s: tonic::Status) -> AppError {
    match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("upload: {s}")),
        tonic::Code::Unauthenticated => AppError::Unauthenticated(format!("upload: {s}")),
        tonic::Code::InvalidArgument | tonic::Code::FailedPrecondition => {
            AppError::Internal(format!("upload rejected: {s}"))
        }
        _ => AppError::Transport(format!("upload: {s}")),
    }
}

/// Map a tonic status from library mutations (create/update/delete).
/// `PermissionDenied` → `Forbidden`; `InvalidArgument` / `NotFound` /
/// `FailedPrecondition` → permanent `Internal`; everything else → `Transport`.
fn map_mutation_err(op: &'static str) -> impl Fn(tonic::Status) -> AppError {
    move |s| match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("{op}: {s}")),
        tonic::Code::Unauthenticated => AppError::Unauthenticated(format!("{op}: {s}")),
        tonic::Code::NotFound
        | tonic::Code::InvalidArgument
        | tonic::Code::FailedPrecondition => {
            AppError::Internal(format!("{op} rejected: {s}"))
        }
        _ => AppError::Transport(format!("{op}: {s}")),
    }
}
/// `map_register_err`: auth/merit rejections do NOT trigger the REST
/// fallback (the server spoke); only transport-level faults do.
fn map_password_err(s: tonic::Status) -> AppError {
    match s.code() {
        tonic::Code::PermissionDenied => AppError::Forbidden(format!("change_password: {s}")),
        tonic::Code::Unauthenticated => {
            AppError::Unauthenticated(format!("change_password: {s}"))
        }
        tonic::Code::InvalidArgument | tonic::Code::NotFound => {
            // Bad new-password / wrong old-password / missing user — surface
            // the server's message. Internal (not Transport) so we don't
            // retry / fall back.
            AppError::Internal(format!("change_password rejected: {s}"))
        }
        _ => AppError::Transport(format!("change_password: {s}")),
    }
}

/// `PermissionTier` → proto `PermissionLevel` wire value. Mirrors
/// `music.auth.v1.PermissionLevel` (USER=1, MANAGER=2, ADMIN=3) without
/// importing the generated enum, so a proto rename can't break this.
fn tier_to_proto_level(tier: super::PermissionTier) -> i32 {
    match tier {
        super::PermissionTier::Admin => 3,
        super::PermissionTier::Manager => 2,
        super::PermissionTier::User => 1,
    }
}

// --- proto -> public model conversions ------------------------------------

fn opt_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn opt_i32(v: i32) -> Option<i64> {
    if v == 0 { None } else { Some(v as i64) }
}

fn opt_i64(v: i64) -> Option<i64> {
    if v == 0 { None } else { Some(v) }
}

fn alias_from_proto(a: super::proto::library::AliasInfo) -> super::AliasInfo {
    super::AliasInfo {
        id: a.id,
        name: a.name,
        sort_name: opt_str(a.sort_name),
        language: opt_str(a.language),
        is_primary: a.is_primary,
    }
}

fn artist_from_proto(a: super::proto::library::Artist) -> Artist {
    Artist {
        id: a.id,
        name: a.name,
        sort_name: opt_str(a.sort_name),
        image_path: opt_str(a.image_path),
        aliases: a.aliases.into_iter().map(alias_from_proto).collect(),
    }
}

fn album_from_proto(a: super::proto::library::Album) -> Album {
    Album {
        id: a.id,
        artist_id: a.artist_id,
        title: a.title,
        release_year: opt_i32(a.release_year),
        cover_path: opt_str(a.cover_path),
        aliases: a.aliases.into_iter().map(alias_from_proto).collect(),
    }
}

fn followed_artist_from_proto(a: npb::FollowedArtist) -> Artist {
    Artist {
        id: a.id,
        name: a.name,
        sort_name: opt_str(a.sort_name),
        image_path: opt_str(a.image_path),
        aliases: Vec::new(),
    }
}

fn notification_from_proto(n: npb::Notification) -> Notification {
    Notification {
        id: n.id,
        kind: n.kind,
        artist_id: opt_str(n.artist_id),
        album_id: opt_str(n.album_id),
        podcast_id: opt_str(n.podcast_id),
        episode_id: opt_str(n.episode_id),
        title: n.title,
        body: opt_str(n.body),
        read: n.read,
        created_at: n.created_at,
    }
}

fn podcast_from_proto(p: ppb::Podcast) -> Podcast {
    Podcast {
        id: p.id,
        feed_url: p.feed_url,
        title: p.title,
        author: opt_str(p.author),
        description: opt_str(p.description),
        image_url: opt_str(p.image_url),
        link: opt_str(p.link),
        language: opt_str(p.language),
        categories: p.categories,
        itunes_id: opt_i64(p.itunes_id),
        podcastindex_id: opt_i64(p.podcastindex_id),
        auto_download: p.auto_download,
        last_refreshed_at: opt_str(p.last_refreshed_at),
    }
}

fn candidate_from_proto(c: ppb::PodcastCandidate) -> PodcastCandidate {
    PodcastCandidate {
        feed_url: c.feed_url,
        title: c.title,
        author: opt_str(c.author),
        description: opt_str(c.description),
        image_url: opt_str(c.image_url),
        categories: c.categories,
        itunes_id: opt_i64(c.itunes_id),
        podcastindex_id: opt_i64(c.podcastindex_id),
    }
}

fn episode_from_proto(e: ppb::Episode) -> PodcastEpisode {
    PodcastEpisode {
        id: e.id,
        podcast_id: e.podcast_id,
        guid: e.guid,
        title: e.title,
        description: opt_str(e.description),
        enclosure_url: e.enclosure_url,
        enclosure_type: opt_str(e.enclosure_type),
        episode_no: opt_i32(e.episode_no),
        season_no: opt_i32(e.season_no),
        duration_ms: opt_i64(e.duration_ms),
        codec: opt_str(e.codec),
        bitrate_kbps: opt_i32(e.bitrate_kbps),
        file_size: opt_i64(e.file_size),
        image_url: opt_str(e.image_url),
        published_at: opt_str(e.published_at),
        downloaded: e.downloaded,
    }
}

fn playlist_from_proto(p: super::proto::playlist::Playlist) -> Playlist {
    Playlist {
        id: p.id,
        owner_id: p.owner_id,
        name: p.name,
    }
}

fn playlist_track_from_proto(t: super::proto::playlist::PlaylistTrack) -> PlaylistTrack {
    PlaylistTrack {
        playlist_id: t.playlist_id,
        track_id: t.track_id,
        position: t.position as i64,
    }
}

fn track_from_proto(t: super::proto::library::Track) -> Track {
    Track {
        id: t.id,
        album_id: t.album_id,
        artist_id: t.artist_id,
        title: t.title,
        track_no: opt_i32(t.track_no),
        disc_no: opt_i32(t.disc_no),
        duration_ms: t.duration_ms,
        codec: t.codec,
        bitrate_kbps: opt_i32(t.bitrate_kbps),
        file_path: t.file_path,
        file_size: opt_i64(t.file_size),
        metadata_json: t.metadata_json,
        is_single_release: t.is_single_release,
    }
}

pub struct GrpcLoginOutcome {
    pub token: String,
    pub user_id: String,
    pub tier: PermissionTier,
    pub expires_at: String,
}

pub struct GrpcWhoAmI {
    pub kind: String,
    pub user_id: String,
    pub username: String,
    pub tier: PermissionTier,
}

/// Attach an `Authorization` header to a gRPC request — same shape as the
/// REST middleware in `server/src/rest/mod.rs`.
fn attach_credential<T>(req: &mut Request<T>, cred: &Credential) -> AppResult<()> {
    let header = match cred {
        Credential::SecretKey(k) => format!("SecretKey {k}"),
        Credential::Bearer(t) => format!("Bearer {t}"),
    };
    let value: MetadataValue<_> = header
        .parse()
        .map_err(|e| AppError::Transport(format!("invalid auth metadata: {e}")))?;
    req.metadata_mut().insert("authorization", value);
    Ok(())
}
