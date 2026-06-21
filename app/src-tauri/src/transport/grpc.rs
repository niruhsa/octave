//! gRPC client over the server's auth service.
//!
//! Connection is lazy and per-call: `Endpoint::connect()` is cheap once the
//! channel is warm (tonic reuses HTTP/2 streams), and constructing a new
//! channel on failure is simpler than keeping state for reconnect logic at
//! this stage. Phase 5 may add a long-lived channel + reconnect strategy.

use std::time::Duration;

use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use tonic::Request;

use super::proto::auth::auth_service_client::AuthServiceClient;
use super::proto::auth::{LoginRequest, LogoutRequest, WhoAmIRequest};
use super::proto::auth::{ChangePasswordRequest, DeleteUserRequest, ListUsersRequest, RegisterRequest, RegisterResponse};
use super::proto::library::library_service_client::LibraryServiceClient;
use super::proto::library::{
    DeleteAlbumRequest, DeleteArtistRequest, DeleteTrackRequest,
    GetAlbumRequest, GetArtistRequest, GetTrackRequest, ListAlbumsByArtistRequest,
    ListArtistsRequest, ListTracksByAlbumRequest, Pagination, SearchRequest,
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
use super::{
    Album, ArchiveUploadResult, Artist, Credential, PermissionTier, Playlist, PlaylistTrack,
    PlaylistWithTracks, RescanReport, ServerConfig, SingleUploadResult, Track, UploadResult,
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
        let endpoint: Endpoint = Endpoint::from_shared(config.grpc_endpoint().to_string())
            .map_err(|e| AppError::Transport(format!("invalid gRPC endpoint: {e}")))?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .tcp_nodelay(true);

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| AppError::Transport(format!("gRPC connect: {e}")))?;
        Ok(Self { channel })
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
        filename: &str,
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
                filename: filename.to_string(),
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

fn artist_from_proto(a: super::proto::library::Artist) -> Artist {
    Artist {
        id: a.id,
        name: a.name,
        sort_name: opt_str(a.sort_name),
    }
}

fn album_from_proto(a: super::proto::library::Album) -> Album {
    Album {
        id: a.id,
        artist_id: a.artist_id,
        title: a.title,
        release_year: opt_i32(a.release_year),
        cover_path: opt_str(a.cover_path),
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
