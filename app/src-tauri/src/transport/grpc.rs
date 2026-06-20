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
use super::proto::library::library_service_client::LibraryServiceClient;
use super::proto::library::{
    ListAlbumsByArtistRequest, ListArtistsRequest, ListTracksByAlbumRequest, Pagination,
    SearchRequest,
};
use super::{
    Album, Artist, Credential, PermissionTier, ServerConfig, Track,
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
