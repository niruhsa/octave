//! REST lyrics routes (Phase 15) — feature parity with the gRPC
//! `LyricsService`. `GET /tracks/:id/lyrics` is any authed user; the
//! refetch/set/clear/scan mutations are Manager-gated. When `LYRICS_ENABLED` is
//! off the read returns `found = false` and `/lyrics/status` reports
//! `enabled = false` (mirrors `/fingerprint/status`).

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::{LyricsService, LyricsView};

pub fn router() -> Router<RestState> {
    Router::new()
        .route(
            "/tracks/:id/lyrics",
            get(get_lyrics).put(set_lyrics).delete(clear_lyrics),
        )
        .route("/tracks/:id/lyrics/refetch", post(refetch_lyrics))
        .route("/lyrics/status", get(status))
        .route("/lyrics/scan", post(scan))
}

#[derive(Serialize)]
struct LineDto {
    ms: u32,
    text: String,
}

#[derive(Serialize, Default)]
struct LyricsDto {
    /// Has renderable lines.
    found: bool,
    synced: bool,
    instrumental: bool,
    source: Option<String>,
    lines: Vec<LineDto>,
    plain: String,
}

#[derive(Serialize, Default)]
struct StatusDto {
    enabled: bool,
    synced: i64,
    plain: i64,
    instrumental: i64,
    missing: i64,
}

#[derive(Deserialize)]
struct SetLyricsBody {
    /// `.lrc` or plain text.
    lrc: String,
}

/// The service or a 400 "disabled" error (mutations only).
fn svc(s: &RestState) -> Result<&LyricsService, ApiError> {
    s.lyrics.as_ref().ok_or_else(|| {
        AppError::InvalidArgument("lyrics are disabled (LYRICS_ENABLED off)".into()).into()
    })
}

fn view_to_dto(v: LyricsView) -> LyricsDto {
    LyricsDto {
        found: v.found,
        synced: v.synced,
        instrumental: v.instrumental,
        source: v.source,
        lines: v
            .lines
            .into_iter()
            .map(|l| LineDto {
                ms: l.ms,
                text: l.text,
            })
            .collect(),
        plain: v.plain,
    }
}

/// `GET /tracks/:id/lyrics` — parsed lyrics (any authed user, read-only).
async fn get_lyrics(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<LyricsDto>, ApiError> {
    c.require(PermissionLevel::User)?;
    let dto = match &s.lyrics {
        Some(l) => view_to_dto(l.get(&c, id).await?),
        None => LyricsDto::default(), // disabled ⇒ graceful "no lyrics"
    };
    Ok(Json(dto))
}

/// `POST /tracks/:id/lyrics/refetch` — force re-resolve (Manager+).
async fn refetch_lyrics(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<LyricsDto>, ApiError> {
    let l = svc(&s)?;
    l.refetch(&c, id).await?;
    Ok(Json(view_to_dto(l.get(&c, id).await?)))
}

/// `PUT /tracks/:id/lyrics` — set manual `.lrc`/text (Manager+).
async fn set_lyrics(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetLyricsBody>,
) -> Result<Json<LyricsDto>, ApiError> {
    let view = svc(&s)?.set_manual(&c, id, body.lrc).await?;
    Ok(Json(view_to_dto(view)))
}

/// `DELETE /tracks/:id/lyrics` — clear (Manager+).
async fn clear_lyrics(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<LyricsDto>, ApiError> {
    let l = svc(&s)?;
    l.clear(&c, id).await?;
    Ok(Json(view_to_dto(l.get(&c, id).await?)))
}

/// `GET /lyrics/status` — library-wide coverage (any authed user).
async fn status(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<StatusDto>, ApiError> {
    c.require(PermissionLevel::User)?;
    let dto = match &s.lyrics {
        Some(l) => {
            let st = l.status().await;
            StatusDto {
                enabled: true,
                synced: st.synced,
                plain: st.plain,
                instrumental: st.instrumental,
                missing: st.missing,
            }
        }
        None => StatusDto::default(),
    };
    Ok(Json(dto))
}

/// `POST /lyrics/scan` — trigger a resolve pass on demand (Manager+).
async fn scan(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<StatusDto>, ApiError> {
    c.require(PermissionLevel::Manager)?;
    let l = svc(&s)?;
    l.run_pass().await;
    let st = l.status().await;
    Ok(Json(StatusDto {
        enabled: true,
        synced: st.synced,
        plain: st.plain,
        instrumental: st.instrumental,
        missing: st.missing,
    }))
}
