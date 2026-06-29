//! gRPC DiscoverService implementation (Phase 11 — recommendations).

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::discover as pb;
use crate::services::RecommendationService;

#[derive(Clone)]
pub struct DiscoverServer {
    pub discover: RecommendationService,
    pub interceptor: AuthInterceptor,
}

impl DiscoverServer {
    pub fn into_service(self) -> pb::discover_service_server::DiscoverServiceServer<Self> {
        pb::discover_service_server::DiscoverServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

/// Parse an optional id field (empty = none).
fn opt_uuid(s: &str, what: &str) -> Result<Option<Uuid>, Status> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(s)
        .map(Some)
        .map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn album_to_pb(a: m::Album) -> pb::DiscAlbum {
    pb::DiscAlbum {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year.unwrap_or(0),
        cover_path: a.cover_path.unwrap_or_default(),
        storage_bytes: a.storage_bytes,
    }
}

fn track_to_pb(t: m::Track) -> pb::DiscTrack {
    pb::DiscTrack {
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
    }
}

#[tonic::async_trait]
impl pb::discover_service_server::DiscoverService for DiscoverServer {
    async fn get_home(
        &self,
        req: Request<pb::GetHomeRequest>,
    ) -> Result<Response<pb::GetHomeResponse>, Status> {
        let caller = self.caller(&req).await?;
        let sections = self.discover.get_home(&caller).await.map_err(map_err)?;
        Ok(Response::new(pb::GetHomeResponse {
            sections: sections
                .into_iter()
                .map(|s| pb::DiscoverSection {
                    id: s.id,
                    title: s.title,
                    albums: s.albums.into_iter().map(album_to_pb).collect(),
                })
                .collect(),
        }))
    }

    async fn get_radio(
        &self,
        req: Request<pb::GetRadioRequest>,
    ) -> Result<Response<pb::GetRadioResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let seed_artist = opt_uuid(&body.seed_artist_id, "artist")?;
        let seed_album = opt_uuid(&body.seed_album_id, "album")?;
        let tracks = self
            .discover
            .get_radio(&caller, seed_artist, seed_album)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetRadioResponse {
            tracks: tracks.into_iter().map(track_to_pb).collect(),
        }))
    }
}
