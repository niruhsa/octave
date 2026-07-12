//! gRPC FavoriteService implementation (Phase 11 — favorites).

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, FavoriteKind};
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::favorite as pb;
use crate::services::FavoritesService;

#[derive(Clone)]
pub struct FavoriteServer {
    pub favorites: FavoritesService,
    pub interceptor: AuthInterceptor,
}

impl FavoriteServer {
    pub fn into_service(self) -> pb::favorite_service_server::FavoriteServiceServer<Self> {
        pb::favorite_service_server::FavoriteServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn parse_kind(s: &str) -> Result<FavoriteKind, Status> {
    FavoriteKind::parse(s)
        .ok_or_else(|| Status::invalid_argument("kind must be track|album|artist"))
}

fn artist_to_pb(a: m::Artist) -> pb::FavArtist {
    pb::FavArtist {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name.unwrap_or_default(),
        image_path: a.image_path.unwrap_or_default(),
        storage_bytes: a.storage_bytes,
    }
}
fn album_to_pb(a: m::Album) -> pb::FavAlbum {
    pb::FavAlbum {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year.unwrap_or(0),
        cover_path: a.cover_path.unwrap_or_default(),
        storage_bytes: a.storage_bytes,
    }
}
fn track_to_pb(t: m::Track) -> pb::FavTrack {
    pb::FavTrack {
        id: t.id.to_string(),
        album_id: t.album_id.to_string(),
        artist_id: t.artist_id.to_string(),
        title: t.title,
        track_no: t.track_no.unwrap_or(0),
        disc_no: t.disc_no.unwrap_or(0),
        duration_ms: t.duration_ms,
        codec: t.codec,
        bitrate_kbps: t.bitrate_kbps.unwrap_or(0),
        file_path: t.file_path,
        file_size: t.file_size.unwrap_or(0),
        metadata_json: t.metadata_json,
        is_single_release: t.is_single_release,
        sample_rate_hz: t.sample_rate_hz.unwrap_or(0),
        bit_depth: t.bit_depth.unwrap_or(0),
        channels: t.channels.unwrap_or(0),
        loudness_lufs: t.loudness_lufs,
        loudness_peak: t.loudness_peak,
        album_loudness_lufs: t.album_loudness_lufs,
    }
}

#[tonic::async_trait]
impl pb::favorite_service_server::FavoriteService for FavoriteServer {
    async fn favorite(
        &self,
        req: Request<pb::FavoriteRequest>,
    ) -> Result<Response<pb::FavoriteStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let kind = parse_kind(&body.kind)?;
        let id = parse_uuid(&body.entity_id, "entity")?;
        self.favorites
            .favorite(&caller, kind, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FavoriteStatusResponse { favorited: true }))
    }

    async fn unfavorite(
        &self,
        req: Request<pb::FavoriteRequest>,
    ) -> Result<Response<pb::FavoriteStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let kind = parse_kind(&body.kind)?;
        let id = parse_uuid(&body.entity_id, "entity")?;
        self.favorites
            .unfavorite(&caller, kind, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FavoriteStatusResponse { favorited: false }))
    }

    async fn is_favorite(
        &self,
        req: Request<pb::FavoriteRequest>,
    ) -> Result<Response<pb::FavoriteStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let kind = parse_kind(&body.kind)?;
        let id = parse_uuid(&body.entity_id, "entity")?;
        let favorited = self
            .favorites
            .is_favorite(&caller, kind, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FavoriteStatusResponse { favorited }))
    }

    async fn list_favorite_tracks(
        &self,
        req: Request<pb::ListFavoritesRequest>,
    ) -> Result<Response<pb::ListFavoriteTracksResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rows = self.favorites.list_tracks(&caller).await.map_err(map_err)?;
        Ok(Response::new(pb::ListFavoriteTracksResponse {
            tracks: rows.into_iter().map(track_to_pb).collect(),
        }))
    }

    async fn list_favorite_albums(
        &self,
        req: Request<pb::ListFavoritesRequest>,
    ) -> Result<Response<pb::ListFavoriteAlbumsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rows = self.favorites.list_albums(&caller).await.map_err(map_err)?;
        Ok(Response::new(pb::ListFavoriteAlbumsResponse {
            albums: rows.into_iter().map(album_to_pb).collect(),
        }))
    }

    async fn list_favorite_artists(
        &self,
        req: Request<pb::ListFavoritesRequest>,
    ) -> Result<Response<pb::ListFavoriteArtistsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rows = self.favorites.list_artists(&caller).await.map_err(map_err)?;
        Ok(Response::new(pb::ListFavoriteArtistsResponse {
            artists: rows.into_iter().map(artist_to_pb).collect(),
        }))
    }

    async fn list_favorite_track_ids(
        &self,
        req: Request<pb::ListFavoritesRequest>,
    ) -> Result<Response<pb::ListFavoriteTrackIdsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let ids = self
            .favorites
            .favorited_track_ids(&caller)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListFavoriteTrackIdsResponse {
            track_ids: ids.into_iter().map(|id| id.to_string()).collect(),
        }))
    }
}
