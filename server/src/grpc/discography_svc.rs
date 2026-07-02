//! gRPC DiscographyService implementation (Phase 14) — parity with the REST
//! discography routes. Manager-gated at the service layer; `None` service
//! (DISCOGRAPHY_ENABLED off) makes `Status` report `enabled = false` and every
//! other RPC return `failed_precondition`.

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{DiscographyIgnore, DiscographyReport, PermissionLevel};
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::discography as pb;
use crate::services::discography::{ArtistCandidate, IgnoreRequest, SyncOutcome};
use crate::services::DiscographyService;
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct DiscographyServer {
    /// `None` when `DISCOGRAPHY_ENABLED` is off.
    pub discography: Option<DiscographyService>,
    pub interceptor: AuthInterceptor,
}

impl DiscographyServer {
    pub fn into_service(self) -> pb::discography_service_server::DiscographyServiceServer<Self> {
        pb::discography_service_server::DiscographyServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }

    /// The service, or `failed_precondition` when discography is disabled.
    fn svc(&self) -> Result<&DiscographyService, Status> {
        self.discography.as_ref().ok_or_else(|| {
            Status::failed_precondition("discography sync is disabled (DISCOGRAPHY_ENABLED off)")
        })
    }
}

fn req_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s.trim()).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn opt_string(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn report_to_pb(r: DiscographyReport) -> pb::DiscographyReport {
    pb::DiscographyReport {
        artist_id: r.artist_id.to_string(),
        provider: r.provider,
        missing_releases: r
            .missing_releases
            .into_iter()
            .map(|m| pb::MissingRelease {
                title: m.title,
                album_type: m.album_type,
                year: m.year.unwrap_or(0),
                provider_id: m.provider_id,
            })
            .collect(),
        incomplete_albums: r
            .incomplete_albums
            .into_iter()
            .map(|a| pb::IncompleteAlbum {
                album_id: a.album_id.to_string(),
                title: a.title,
                release_group_id: a.release_group_id,
                missing_tracks: a
                    .missing_tracks
                    .into_iter()
                    .map(|t| pb::MissingTrack {
                        title: t.title,
                        position: t.position.unwrap_or(0),
                        disc_no: t.disc_no.unwrap_or(0),
                        recording_id: t.recording_id.unwrap_or_default(),
                        title_key: t.title_key,
                    })
                    .collect(),
            })
            .collect(),
        missing_release_count: r.missing_release_count,
        incomplete_album_count: r.incomplete_album_count,
        generated_at: rfc3339(r.generated_at),
    }
}

fn candidate_to_pb(c: &ArtistCandidate) -> pb::Candidate {
    pb::Candidate {
        provider_id: c.provider_id.clone(),
        name: c.name.clone(),
        disambiguation: c.disambiguation.clone().unwrap_or_default(),
        score: c.score as u32,
    }
}

fn ignore_to_pb(i: DiscographyIgnore) -> pb::Ignore {
    pb::Ignore {
        id: i.id.to_string(),
        artist_id: i.artist_id.to_string(),
        scope: i.scope,
        release_group_id: i.release_group_id.to_string(),
        recording_id: i.recording_id.map(|u| u.to_string()).unwrap_or_default(),
        title_key: i.title_key.unwrap_or_default(),
        label: i.label,
        created_at: rfc3339(i.created_at),
    }
}

#[tonic::async_trait]
impl pb::discography_service_server::DiscographyService for DiscographyServer {
    async fn get_report(
        &self,
        req: Request<pb::ArtistIdRequest>,
    ) -> Result<Response<pb::GetReportResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().artist_id, "artist")?;
        let report = self.svc()?.report(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::GetReportResponse {
            has_report: report.is_some(),
            report: report.map(report_to_pb),
        }))
    }

    async fn sync_artist(
        &self,
        req: Request<pb::ArtistIdRequest>,
    ) -> Result<Response<pb::SyncResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().artist_id, "artist")?;
        let out = self.svc()?.sync_artist(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(match out {
            SyncOutcome::Report(r) => pb::SyncResponse {
                status: "report".to_string(),
                report: Some(report_to_pb(r)),
                candidates: Vec::new(),
            },
            SyncOutcome::NeedsResolution(cands) => pb::SyncResponse {
                status: "needs_resolution".to_string(),
                report: None,
                candidates: cands.iter().map(candidate_to_pb).collect(),
            },
        }))
    }

    async fn get_candidates(
        &self,
        req: Request<pb::ArtistIdRequest>,
    ) -> Result<Response<pb::CandidateList>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().artist_id, "artist")?;
        let cands = self.svc()?.candidates(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::CandidateList {
            candidates: cands.iter().map(candidate_to_pb).collect(),
        }))
    }

    async fn resolve(
        &self,
        req: Request<pb::ResolveRequest>,
    ) -> Result<Response<pb::OkResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = req_uuid(&body.artist_id, "artist")?;
        let provider_id = opt_string(body.mbid);
        self.svc()?
            .resolve(&caller, id, provider_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::OkResponse { ok: true }))
    }

    async fn list_ignores(
        &self,
        req: Request<pb::ArtistIdRequest>,
    ) -> Result<Response<pb::IgnoreList>, Status> {
        let caller = self.caller(&req).await?;
        let id = req_uuid(&req.into_inner().artist_id, "artist")?;
        let ignores = self.svc()?.list_ignores(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::IgnoreList {
            ignores: ignores.into_iter().map(ignore_to_pb).collect(),
        }))
    }

    async fn add_ignore(
        &self,
        req: Request<pb::AddIgnoreRequest>,
    ) -> Result<Response<pb::GetReportResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = req_uuid(&body.artist_id, "artist")?;
        let release_group_id = body.release_group_id.trim().to_string();
        if release_group_id.is_empty() {
            return Err(Status::invalid_argument("release_group_id is required"));
        }
        let report = self
            .svc()?
            .ignore(
                &caller,
                id,
                IgnoreRequest {
                    scope: body.scope,
                    release_group_id,
                    recording_id: opt_string(body.recording_id),
                    title_key: opt_string(body.title_key),
                    label: body.label,
                },
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetReportResponse {
            has_report: true,
            report: Some(report_to_pb(report)),
        }))
    }

    async fn remove_ignore(
        &self,
        req: Request<pb::RemoveIgnoreRequest>,
    ) -> Result<Response<pb::GetReportResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = req_uuid(&body.artist_id, "artist")?;
        let ignore_id = req_uuid(&body.ignore_id, "ignore")?;
        let report = self
            .svc()?
            .unignore(&caller, id, ignore_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetReportResponse {
            has_report: true,
            report: Some(report_to_pb(report)),
        }))
    }

    async fn status(
        &self,
        req: Request<pb::StatusRequest>,
    ) -> Result<Response<pb::StatusResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::Manager).map_err(map_err)?;
        let resp = match &self.discography {
            Some(d) => {
                let st = d.status().await;
                pb::StatusResponse {
                    enabled: st.enabled,
                    provider: st.provider,
                    artists_total: st.artists_total,
                    matched: st.matched,
                    unresolved: st.unresolved,
                    ignored: st.ignored,
                }
            }
            None => pb::StatusResponse {
                enabled: false,
                provider: String::new(),
                artists_total: 0,
                matched: 0,
                unresolved: 0,
                ignored: 0,
            },
        };
        Ok(Response::new(resp))
    }

    async fn sync_all(
        &self,
        req: Request<pb::SyncAllRequest>,
    ) -> Result<Response<pb::SyncAllResponse>, Status> {
        let caller = self.caller(&req).await?;
        caller.require(PermissionLevel::Manager).map_err(map_err)?;
        let report = self.svc()?.run_pass().await;
        Ok(Response::new(pb::SyncAllResponse {
            synced: report.synced,
            skipped_fresh: report.skipped_fresh,
            failed: report.failed,
            total: report.total,
        }))
    }
}
