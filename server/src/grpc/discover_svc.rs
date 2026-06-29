//! gRPC DiscoverService implementation (Phase 11 — recommendations).

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, PermissionLevel};
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::discover as pb;
use crate::services::recommendation::{PLAYLIST_REC_DEFAULT, SIMILAR_DEFAULT};
use crate::services::{FingerprintService, RecommendationService};

#[derive(Clone)]
pub struct DiscoverServer {
    pub discover: RecommendationService,
    /// Acoustic fingerprinting (Phase 12). `None` when `FINGERPRINT_ENABLED` is
    /// off — status reports `enabled = false` and scan is unavailable.
    pub fingerprint: Option<FingerprintService>,
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
        let seed_track = opt_uuid(&body.seed_track_id, "track")?;
        let tracks = self
            .discover
            .get_radio(&caller, seed_artist, seed_album, seed_track)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetRadioResponse {
            tracks: tracks.into_iter().map(track_to_pb).collect(),
        }))
    }

    async fn get_similar_tracks(
        &self,
        req: Request<pb::GetSimilarRequest>,
    ) -> Result<Response<pb::GetRadioResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let track_id = opt_uuid(&body.track_id, "track")?
            .ok_or_else(|| Status::invalid_argument("track_id is required"))?;
        let limit = if body.limit <= 0 {
            SIMILAR_DEFAULT
        } else {
            body.limit as usize
        };
        let tracks = self
            .discover
            .similar_tracks(&caller, track_id, limit)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetRadioResponse {
            tracks: tracks.into_iter().map(track_to_pb).collect(),
        }))
    }

    async fn recommend_for_playlist(
        &self,
        req: Request<pb::RecommendForPlaylistRequest>,
    ) -> Result<Response<pb::GetRadioResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let mut seeds = Vec::with_capacity(body.seed_track_ids.len());
        for s in &body.seed_track_ids {
            if let Some(id) = opt_uuid(s, "seed track")? {
                seeds.push(id);
            }
        }
        let limit = if body.limit <= 0 {
            PLAYLIST_REC_DEFAULT
        } else {
            body.limit as usize
        };
        let tracks = self
            .discover
            .recommend_for_playlist(&caller, &seeds, limit)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetRadioResponse {
            tracks: tracks.into_iter().map(track_to_pb).collect(),
        }))
    }

    async fn fingerprint_status(
        &self,
        req: Request<pb::FingerprintStatusRequest>,
    ) -> Result<Response<pb::FingerprintStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::User).map_err(map_err)?;
        let resp = match &self.fingerprint {
            Some(fp) => {
                let s = fp.status().await;
                pb::FingerprintStatusResponse {
                    analyzed: s.analyzed,
                    total: s.total,
                    model_version: s.model_version,
                    enabled: true,
                }
            }
            None => pb::FingerprintStatusResponse {
                analyzed: 0,
                total: 0,
                model_version: String::new(),
                enabled: false,
            },
        };
        Ok(Response::new(resp))
    }

    async fn fingerprint_scan(
        &self,
        req: Request<pb::FingerprintScanRequest>,
    ) -> Result<Response<pb::FingerprintStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::Manager).map_err(map_err)?;
        let fp = self
            .fingerprint
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("fingerprinting is disabled"))?;
        fp.run_pass().await;
        let s = fp.status().await;
        Ok(Response::new(pb::FingerprintStatusResponse {
            analyzed: s.analyzed,
            total: s.total,
            model_version: s.model_version,
            enabled: true,
        }))
    }
}
