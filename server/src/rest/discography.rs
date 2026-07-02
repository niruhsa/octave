//! REST discography routes (Phase 14) — Manager-gated reconciliation against an
//! online metadata provider. Feature parity target for a future gRPC service;
//! REST is the verifiable path for Phase A.
//!
//! All routes require Manager. When `DISCOGRAPHY_ENABLED` is off the service is
//! `None`: `/discography/status` reports `enabled = false` and the rest return
//! `400` ("disabled").

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, PermissionLevel};
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::discography::{ArtistCandidate, IgnoreRequest, SyncOutcome};
use crate::services::DiscographyService;
use crate::time_fmt::rfc3339;

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/artists/:id/discography", get(get_report))
        .route("/artists/:id/discography/sync", post(sync))
        .route("/artists/:id/discography/candidates", get(candidates))
        .route("/artists/:id/discography/resolve", post(resolve))
        .route(
            "/artists/:id/discography/ignores",
            get(list_ignores).post(add_ignore),
        )
        .route(
            "/artists/:id/discography/ignores/:ignore_id",
            delete(remove_ignore),
        )
        .route("/discography/status", get(status))
        .route("/discography/sync-all", post(sync_all))
}

/// Resolve the service or a 400 "disabled" error.
fn svc(s: &RestState) -> Result<&DiscographyService, ApiError> {
    s.discography.as_ref().ok_or_else(|| {
        AppError::InvalidArgument("discography sync is disabled (DISCOGRAPHY_ENABLED off)".into())
            .into()
    })
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CandidateDto {
    provider_id: String,
    name: String,
    disambiguation: Option<String>,
    score: u8,
}
fn candidate_dto(c: &ArtistCandidate) -> CandidateDto {
    CandidateDto {
        provider_id: c.provider_id.clone(),
        name: c.name.clone(),
        disambiguation: c.disambiguation.clone(),
        score: c.score,
    }
}

#[derive(Serialize)]
struct StatusDto {
    enabled: bool,
    provider: String,
    artists_total: i64,
    matched: i64,
    unresolved: i64,
    ignored: i64,
}

/// Report DTO — mirrors [`m::DiscographyReport`] but renders `generated_at` as an
/// RFC-3339 string (the codebase convention; a raw `OffsetDateTime` would
/// serialize in `time`'s opaque default format). The nested release/track types
/// carry no timestamps, so they serialize directly.
#[derive(Serialize)]
struct ReportDto {
    artist_id: String,
    provider: String,
    missing_releases: Vec<m::MissingRelease>,
    incomplete_albums: Vec<m::IncompleteAlbum>,
    missing_release_count: i32,
    incomplete_album_count: i32,
    generated_at: String,
}
fn report_dto(r: m::DiscographyReport) -> ReportDto {
    ReportDto {
        artist_id: r.artist_id.to_string(),
        provider: r.provider,
        missing_releases: r.missing_releases,
        incomplete_albums: r.incomplete_albums,
        missing_release_count: r.missing_release_count,
        incomplete_album_count: r.incomplete_album_count,
        generated_at: rfc3339(r.generated_at),
    }
}

/// Ignore DTO — `created_at` as RFC-3339; ids as strings.
#[derive(Serialize)]
struct IgnoreDto {
    id: String,
    artist_id: String,
    scope: String,
    release_group_id: String,
    recording_id: Option<String>,
    title_key: Option<String>,
    label: String,
    created_at: String,
}
fn ignore_dto(i: m::DiscographyIgnore) -> IgnoreDto {
    IgnoreDto {
        id: i.id.to_string(),
        artist_id: i.artist_id.to_string(),
        scope: i.scope,
        release_group_id: i.release_group_id.to_string(),
        recording_id: i.recording_id.map(|u| u.to_string()),
        title_key: i.title_key,
        label: i.label,
        created_at: rfc3339(i.created_at),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /artists/:id/discography` — the cached report (`{ "report": … | null }`).
async fn get_report(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let report = svc(&s)?.report(&c, id).await?;
    Ok(Json(serde_json::json!({ "report": report.map(report_dto) })))
}

/// `POST /artists/:id/discography/sync` — trigger a sync (slow: provider I/O).
async fn sync(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let out = svc(&s)?.sync_artist(&c, id).await?;
    Ok(Json(match out {
        SyncOutcome::Report(report) => serde_json::json!({
            "status": "report",
            "report": report_dto(report),
        }),
        SyncOutcome::NeedsResolution(cands) => serde_json::json!({
            "status": "needs_resolution",
            "candidates": cands.iter().map(candidate_dto).collect::<Vec<_>>(),
        }),
    }))
}

/// `GET /artists/:id/discography/candidates` — provider candidates to disambiguate.
async fn candidates(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cands = svc(&s)?.candidates(&c, id).await?;
    Ok(Json(serde_json::json!({
        "candidates": cands.iter().map(candidate_dto).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct ResolveBody {
    /// The chosen provider (MusicBrainz) artist id; omit/empty → set `ignored`.
    #[serde(default)]
    mbid: Option<String>,
}

/// `POST /artists/:id/discography/resolve` — pin the match, or ignore the artist.
async fn resolve(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let provider_id = body
        .mbid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    svc(&s)?.resolve(&c, id, provider_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /artists/:id/discography/ignores` — the suppression list.
async fn list_ignores(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ignores = svc(&s)?.list_ignores(&c, id).await?;
    Ok(Json(serde_json::json!({
        "ignores": ignores.into_iter().map(ignore_dto).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct IgnoreBody {
    scope: String,
    release_group_id: String,
    #[serde(default)]
    recording_id: Option<String>,
    #[serde(default)]
    title_key: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// `POST /artists/:id/discography/ignores` — suppress a release/track. Returns
/// the re-filtered report.
async fn add_ignore(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<IgnoreBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let release_group_id = body.release_group_id.trim().to_string();
    if release_group_id.is_empty() {
        return Err(AppError::InvalidArgument("release_group_id is required".into()).into());
    }
    let recording_id = body
        .recording_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let title_key = body
        .title_key
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let report = svc(&s)?
        .ignore(
            &c,
            id,
            IgnoreRequest {
                scope: body.scope,
                release_group_id,
                recording_id,
                title_key,
                label: body.label.unwrap_or_default(),
            },
        )
        .await?;
    Ok(Json(serde_json::json!({ "report": report_dto(report) })))
}

/// `DELETE /artists/:id/discography/ignores/:ignore_id` — un-ignore; returns the
/// re-filtered report.
async fn remove_ignore(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path((id, ignore_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let report = svc(&s)?.unignore(&c, id, ignore_id).await?;
    Ok(Json(serde_json::json!({ "report": report_dto(report) })))
}

/// `GET /discography/status` — library-wide coverage (Manager).
async fn status(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<StatusDto>, ApiError> {
    c.require(PermissionLevel::Manager)?;
    let dto = match &s.discography {
        Some(d) => {
            let st = d.status().await;
            StatusDto {
                enabled: st.enabled,
                provider: st.provider,
                artists_total: st.artists_total,
                matched: st.matched,
                unresolved: st.unresolved,
                ignored: st.ignored,
            }
        }
        None => StatusDto {
            enabled: false,
            provider: String::new(),
            artists_total: 0,
            matched: 0,
            unresolved: 0,
            ignored: 0,
        },
    };
    Ok(Json(dto))
}

/// `POST /discography/sync-all` — background-style re-sync of every matched
/// artist (Manager). Rate-limited by the provider; returns the pass summary.
async fn sync_all(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<serde_json::Value>, ApiError> {
    c.require(PermissionLevel::Manager)?;
    let d = svc(&s)?;
    let report = d.run_pass().await;
    Ok(Json(serde_json::json!({
        "synced": report.synced,
        "skipped_fresh": report.skipped_fresh,
        "failed": report.failed,
        "total": report.total,
    })))
}
