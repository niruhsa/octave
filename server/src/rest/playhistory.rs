//! REST play-history routes — feature parity with the gRPC
//! `PlayHistoryService` (Phase 11).

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, Request, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::db::repo::{ArtistPlayStat, TrackPlayStat};
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::PlayInput;
use crate::time_fmt::rfc3339;

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/history", post(record).get(list_recent))
        .route("/history/stats", get(get_stats))
}

// ---------------------------------------------------------------------------
// Helpers / DTOs
// ---------------------------------------------------------------------------

fn id(req: &Request<Body>) -> Result<Identity, ApiError> {
    req.extensions()
        .get::<Identity>()
        .cloned()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()).into())
}

#[derive(Deserialize)]
pub struct PlayInputDto {
    pub track_id: Uuid,
    #[serde(default)]
    pub ms_played: i64,
    #[serde(default)]
    pub completed: bool,
    /// RFC3339; omitted/empty → server stamps receipt time.
    pub played_at: Option<String>,
}

#[derive(Deserialize)]
pub struct RecordBody {
    pub events: Vec<PlayInputDto>,
}

#[derive(Serialize)]
pub struct PlayEventDto {
    pub id: String,
    pub track_id: Option<String>,
    pub artist_id: Option<String>,
    pub album_id: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub ms_played: i64,
    pub completed: bool,
    pub played_at: String,
}
fn event_dto(p: m::PlayEvent) -> PlayEventDto {
    PlayEventDto {
        id: p.id.to_string(),
        track_id: p.track_id.map(|id| id.to_string()),
        artist_id: p.artist_id.map(|id| id.to_string()),
        album_id: p.album_id.map(|id| id.to_string()),
        track_title: p.track_title,
        artist_name: p.artist_name,
        ms_played: p.ms_played,
        completed: p.completed,
        played_at: rfc3339(p.played_at),
    }
}

#[derive(Serialize)]
pub struct ListRecentDto {
    pub events: Vec<PlayEventDto>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct TrackStatDto {
    pub track_id: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub plays: i64,
}
fn track_stat_dto(s: TrackPlayStat) -> TrackStatDto {
    TrackStatDto {
        track_id: s.track_id.map(|id| id.to_string()),
        track_title: s.track_title,
        artist_name: s.artist_name,
        plays: s.plays,
    }
}

#[derive(Serialize)]
pub struct ArtistStatDto {
    pub artist_id: Option<String>,
    pub artist_name: String,
    pub plays: i64,
}
fn artist_stat_dto(s: ArtistPlayStat) -> ArtistStatDto {
    ArtistStatDto {
        artist_id: s.artist_id.map(|id| id.to_string()),
        artist_name: s.artist_name,
        plays: s.plays,
    }
}

#[derive(Serialize)]
pub struct StatsDto {
    pub top_tracks: Vec<TrackStatDto>,
    pub top_artists: Vec<ArtistStatDto>,
    pub total_plays: i64,
    pub total_ms: i64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn record(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = id(&req)?;
    let body: RecordBody = crate::rest::parse_json(req).await?;
    let mut events = Vec::with_capacity(body.events.len());
    for e in body.events {
        let played_at = match e.played_at.as_deref() {
            Some(s) if !s.trim().is_empty() => Some(
                OffsetDateTime::parse(s, &Rfc3339)
                    .map_err(|_| AppError::InvalidArgument("invalid played_at (want RFC3339)".into()))?,
            ),
            _ => None,
        };
        events.push(PlayInput {
            track_id: e.track_id,
            ms_played: e.ms_played,
            completed: e.completed,
            played_at,
        });
    }
    let recorded = state.play_history.record(&caller, &events).await?;
    Ok(Json(serde_json::json!({ "recorded": recorded })))
}

#[derive(Deserialize)]
pub struct RecentQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

async fn list_recent(
    State(state): State<RestState>,
    Query(q): Query<RecentQuery>,
    req: Request<Body>,
) -> Result<Json<ListRecentDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.play_history.recent(&caller, q.limit, q.offset).await?;
    let total = rows.len() as i64;
    Ok(Json(ListRecentDto {
        events: rows.into_iter().map(event_dto).collect(),
        total,
    }))
}

#[derive(Deserialize)]
pub struct StatsQuery {
    pub window_days: Option<i64>,
    pub limit: Option<i64>,
}

async fn get_stats(
    State(state): State<RestState>,
    Query(q): Query<StatsQuery>,
    req: Request<Body>,
) -> Result<Json<StatsDto>, ApiError> {
    let caller = id(&req)?;
    let stats = state
        .play_history
        .stats(&caller, q.window_days, q.limit)
        .await?;
    Ok(Json(StatsDto {
        top_tracks: stats.top_tracks.into_iter().map(track_stat_dto).collect(),
        top_artists: stats.top_artists.into_iter().map(artist_stat_dto).collect(),
        total_plays: stats.totals.total_plays,
        total_ms: stats.totals.total_ms,
    }))
}
