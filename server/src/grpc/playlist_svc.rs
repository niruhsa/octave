//! gRPC PlaylistService implementation.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::playlist as pb;
use crate::services::PlaylistService;
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct PlaylistServer {
    pub playlists: PlaylistService,
    pub interceptor: AuthInterceptor,
}

impl PlaylistServer {
    pub fn into_service(self) -> pb::playlist_service_server::PlaylistServiceServer<Self> {
        pb::playlist_service_server::PlaylistServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn playlist_to_pb(p: m::Playlist) -> pb::Playlist {
    pb::Playlist {
        id: p.id.to_string(),
        owner_id: p.owner_id.to_string(),
        name: p.name,
        created_at: rfc3339(p.created_at),
        updated_at: rfc3339(p.updated_at),
    }
}

fn track_to_pb(t: m::PlaylistTrack) -> pb::PlaylistTrack {
    pb::PlaylistTrack {
        playlist_id: t.playlist_id.to_string(),
        track_id: t.track_id.to_string(),
        position: t.position,
        added_at: rfc3339(t.added_at),
    }
}

#[tonic::async_trait]
impl pb::playlist_service_server::PlaylistService for PlaylistServer {
    async fn create_playlist(
        &self,
        req: Request<pb::CreatePlaylistRequest>,
    ) -> Result<Response<pb::Playlist>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let p = self
            .playlists
            .create(&caller, &body.name)
            .await
            .map_err(map_err)?;
        Ok(Response::new(playlist_to_pb(p)))
    }

    async fn get_playlist(
        &self,
        req: Request<pb::GetPlaylistRequest>,
    ) -> Result<Response<pb::PlaylistWithTracks>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "playlist")?;
        let view = self
            .playlists
            .get_with_tracks(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::PlaylistWithTracks {
            playlist: Some(playlist_to_pb(view.playlist)),
            tracks: view.tracks.into_iter().map(track_to_pb).collect(),
        }))
    }

    async fn rename_playlist(
        &self,
        req: Request<pb::RenamePlaylistRequest>,
    ) -> Result<Response<pb::Playlist>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.id, "playlist")?;
        let p = self
            .playlists
            .rename(&caller, id, &body.name)
            .await
            .map_err(map_err)?;
        Ok(Response::new(playlist_to_pb(p)))
    }

    async fn delete_playlist(
        &self,
        req: Request<pb::DeletePlaylistRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "playlist")?;
        let deleted = self
            .playlists
            .delete(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse { deleted }))
    }

    async fn list_my_playlists(
        &self,
        req: Request<pb::ListMyPlaylistsRequest>,
    ) -> Result<Response<pb::ListPlaylistsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let rows = self.playlists.list_mine(&caller).await.map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListPlaylistsResponse {
            playlists: rows.into_iter().map(playlist_to_pb).collect(),
            total,
        }))
    }

    async fn list_playlists_for_owner(
        &self,
        req: Request<pb::ListPlaylistsForOwnerRequest>,
    ) -> Result<Response<pb::ListPlaylistsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let owner = parse_uuid(&req.into_inner().owner_id, "owner")?;
        let rows = self
            .playlists
            .list_for_owner(&caller, owner)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListPlaylistsResponse {
            playlists: rows.into_iter().map(playlist_to_pb).collect(),
            total,
        }))
    }

    async fn list_playlist_tracks(
        &self,
        req: Request<pb::ListPlaylistTracksRequest>,
    ) -> Result<Response<pb::ListPlaylistTracksResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().playlist_id, "playlist")?;
        let rows = self
            .playlists
            .list_tracks(&caller, id)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListPlaylistTracksResponse {
            tracks: rows.into_iter().map(track_to_pb).collect(),
            total,
        }))
    }

    async fn add_playlist_track(
        &self,
        req: Request<pb::AddPlaylistTrackRequest>,
    ) -> Result<Response<pb::PlaylistTrack>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let playlist_id = parse_uuid(&body.playlist_id, "playlist")?;
        let track_id = parse_uuid(&body.track_id, "track")?;
        let row = if body.position <= 0 {
            self.playlists
                .add_track(&caller, playlist_id, track_id)
                .await
        } else {
            self.playlists
                .insert_track(&caller, playlist_id, track_id, body.position)
                .await
        }
        .map_err(map_err)?;
        Ok(Response::new(track_to_pb(row)))
    }

    async fn remove_playlist_track(
        &self,
        req: Request<pb::RemovePlaylistTrackRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let playlist_id = parse_uuid(&body.playlist_id, "playlist")?;
        let removed = self
            .playlists
            .remove_track_at(&caller, playlist_id, body.position)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse {
            deleted: removed.is_some(),
        }))
    }

    async fn reorder_playlist_track(
        &self,
        req: Request<pb::ReorderPlaylistTrackRequest>,
    ) -> Result<Response<pb::ReorderResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let playlist_id = parse_uuid(&body.playlist_id, "playlist")?;
        self.playlists
            .reorder(&caller, playlist_id, body.from_position, body.to_position)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ReorderResponse { moved: true }))
    }
}
