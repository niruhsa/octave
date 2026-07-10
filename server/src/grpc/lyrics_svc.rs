//! gRPC LyricsService implementation (Phase 15) — parity with the REST lyrics
//! routes. Reads are any authed user; refetch/set/clear/scan are Manager-gated
//! at the service layer. `None` service (LYRICS_ENABLED off) makes `Status`
//! report `enabled = false` and every other RPC return `failed_precondition`.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::lyrics as pb;
use crate::services::{LyricsService, LyricsView};

#[derive(Clone)]
pub struct LyricsServer {
    /// `None` when `LYRICS_ENABLED` is off.
    pub lyrics: Option<LyricsService>,
    pub interceptor: AuthInterceptor,
}

impl LyricsServer {
    pub fn into_service(self) -> pb::lyrics_service_server::LyricsServiceServer<Self> {
        pb::lyrics_service_server::LyricsServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }

    /// The service, or `failed_precondition` when lyrics are disabled.
    fn svc(&self) -> Result<&LyricsService, Status> {
        self.lyrics
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("lyrics are disabled (LYRICS_ENABLED off)"))
    }
}

fn req_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s.trim()).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn view_to_pb(v: LyricsView) -> pb::GetLyricsResponse {
    pb::GetLyricsResponse {
        found: v.found,
        synced: v.synced,
        instrumental: v.instrumental,
        source: v.source.unwrap_or_default(),
        lines: v
            .lines
            .into_iter()
            .map(|l| pb::LyricLine {
                ms: l.ms,
                text: l.text,
            })
            .collect(),
        plain: v.plain,
    }
}

#[tonic::async_trait]
impl pb::lyrics_service_server::LyricsService for LyricsServer {
    async fn get_lyrics(
        &self,
        req: Request<pb::GetLyricsRequest>,
    ) -> Result<Response<pb::GetLyricsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().track_id, "track")?;
        // Graceful when disabled (found=false), mirroring the REST route so the
        // client shows "no lyrics" instead of an error.
        let resp = match &self.lyrics {
            Some(l) => view_to_pb(l.get(&caller, id).await.map_err(map_err)?),
            None => pb::GetLyricsResponse::default(),
        };
        Ok(Response::new(resp))
    }

    async fn refetch_lyrics(
        &self,
        req: Request<pb::RefetchLyricsRequest>,
    ) -> Result<Response<pb::GetLyricsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().track_id, "track")?;
        let svc = self.svc()?;
        svc.refetch(&caller, id).await.map_err(map_err)?;
        let view = svc.get(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn set_lyrics(
        &self,
        req: Request<pb::SetLyricsRequest>,
    ) -> Result<Response<pb::GetLyricsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let msg = req.into_inner();
        let id = req_uuid(&msg.track_id, "track")?;
        let view = self
            .svc()?
            .set_manual(&caller, id, msg.lrc)
            .await
            .map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn clear_lyrics(
        &self,
        req: Request<pb::ClearLyricsRequest>,
    ) -> Result<Response<pb::GetLyricsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().track_id, "track")?;
        let svc = self.svc()?;
        svc.clear(&caller, id).await.map_err(map_err)?;
        let view = svc.get(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn status(
        &self,
        req: Request<pb::LyricsStatusRequest>,
    ) -> Result<Response<pb::LyricsStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::User).map_err(map_err)?;
        let resp = match &self.lyrics {
            Some(l) => {
                let s = l.status().await;
                pb::LyricsStatusResponse {
                    enabled: true,
                    synced: s.synced,
                    plain: s.plain,
                    instrumental: s.instrumental,
                    missing: s.missing,
                }
            }
            None => pb::LyricsStatusResponse {
                enabled: false,
                synced: 0,
                plain: 0,
                instrumental: 0,
                missing: 0,
            },
        };
        Ok(Response::new(resp))
    }

    async fn scan(
        &self,
        req: Request<pb::LyricsScanRequest>,
    ) -> Result<Response<pb::LyricsStatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::Manager).map_err(map_err)?;
        let l = self.svc()?;
        l.run_pass().await;
        let s = l.status().await;
        Ok(Response::new(pb::LyricsStatusResponse {
            enabled: true,
            synced: s.synced,
            plain: s.plain,
            instrumental: s.instrumental,
            missing: s.missing,
        }))
    }
}
