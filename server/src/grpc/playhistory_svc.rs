//! gRPC PlayHistoryService implementation (Phase 11 — play history).

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::repo::{ArtistPlayStat, TrackPlayStat};
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::playhistory as pb;
use crate::services::{PlayHistoryService, PlayInput};
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct PlayHistoryServer {
    pub plays: PlayHistoryService,
    pub interceptor: AuthInterceptor,
}

impl PlayHistoryServer {
    pub fn into_service(self) -> pb::play_history_service_server::PlayHistoryServiceServer<Self> {
        pb::play_history_service_server::PlayHistoryServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

/// Parse an optional RFC3339 timestamp. Empty → `None` (server stamps now()).
fn parse_played_at(s: &str) -> Result<Option<OffsetDateTime>, Status> {
    if s.trim().is_empty() {
        return Ok(None);
    }
    OffsetDateTime::parse(s, &Rfc3339)
        .map(Some)
        .map_err(|_| Status::invalid_argument("invalid played_at (want RFC3339)"))
}

/// 0 (the proto default) means "unset" → let the service apply its default.
fn opt_i64(v: i32) -> Option<i64> {
    if v > 0 { Some(v as i64) } else { None }
}

fn event_to_pb(p: crate::db::models::PlayEvent) -> pb::PlayEvent {
    pb::PlayEvent {
        id: p.id.to_string(),
        track_id: p.track_id.map(|id| id.to_string()).unwrap_or_default(),
        artist_id: p.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        album_id: p.album_id.map(|id| id.to_string()).unwrap_or_default(),
        track_title: p.track_title,
        artist_name: p.artist_name,
        ms_played: p.ms_played,
        completed: p.completed,
        played_at: rfc3339(p.played_at),
    }
}

fn track_stat_to_pb(s: TrackPlayStat) -> pb::TrackStat {
    pb::TrackStat {
        track_id: s.track_id.map(|id| id.to_string()).unwrap_or_default(),
        track_title: s.track_title,
        artist_name: s.artist_name,
        plays: s.plays,
    }
}

fn artist_stat_to_pb(s: ArtistPlayStat) -> pb::ArtistStat {
    pb::ArtistStat {
        artist_id: s.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        artist_name: s.artist_name,
        plays: s.plays,
    }
}

#[tonic::async_trait]
impl pb::play_history_service_server::PlayHistoryService for PlayHistoryServer {
    async fn record_plays(
        &self,
        req: Request<pb::RecordPlaysRequest>,
    ) -> Result<Response<pb::RecordPlaysResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let mut events = Vec::with_capacity(body.events.len());
        for e in body.events {
            events.push(PlayInput {
                track_id: parse_uuid(&e.track_id, "track")?,
                ms_played: e.ms_played,
                completed: e.completed,
                played_at: parse_played_at(&e.played_at)?,
            });
        }
        let recorded = self.plays.record(&caller, &events).await.map_err(map_err)? as i64;
        Ok(Response::new(pb::RecordPlaysResponse { recorded }))
    }

    async fn list_recent(
        &self,
        req: Request<pb::ListRecentRequest>,
    ) -> Result<Response<pb::ListRecentResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let rows = self
            .plays
            .recent(&caller, opt_i64(body.limit), opt_i64(body.offset))
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListRecentResponse {
            events: rows.into_iter().map(event_to_pb).collect(),
            total,
        }))
    }

    async fn get_stats(
        &self,
        req: Request<pb::GetStatsRequest>,
    ) -> Result<Response<pb::GetStatsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let stats = self
            .plays
            .stats(&caller, opt_i64(body.window_days), opt_i64(body.limit))
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::GetStatsResponse {
            top_tracks: stats.top_tracks.into_iter().map(track_stat_to_pb).collect(),
            top_artists: stats
                .top_artists
                .into_iter()
                .map(artist_stat_to_pb)
                .collect(),
            total_plays: stats.totals.total_plays,
            total_ms: stats.totals.total_ms,
        }))
    }
}
