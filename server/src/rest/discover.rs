//! REST discover routes — feature parity with the gRPC `DiscoverService`
//! (Phase 11). `GET /discover` (personalized album shelves) + `GET
//! /discover/radio` (a seeded track queue).

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, PermissionLevel};
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::recommendation::{PLAYLIST_REC_DEFAULT, SIMILAR_DEFAULT};

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/discover", get(home))
        .route("/discover/radio", get(radio))
        .route("/discover/recommendations", post(recommendations))
        .route("/tracks/:id/similar", get(similar))
        .route("/fingerprint/status", get(fingerprint_status))
        .route("/fingerprint/scan", post(fingerprint_scan))
}

#[derive(Serialize)]
pub struct DiscAlbumDto {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
    pub storage_bytes: i64,
}
fn album_dto(a: m::Album) -> DiscAlbumDto {
    DiscAlbumDto {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year,
        cover_path: a.cover_path,
        storage_bytes: a.storage_bytes,
    }
}

#[derive(Serialize)]
pub struct DiscTrackDto {
    pub id: String,
    pub album_id: String,
    pub artist_id: String,
    pub title: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i32>,
    pub file_path: String,
    pub file_size: Option<i64>,
    pub sample_rate_hz: Option<i32>,
    pub bit_depth: Option<i32>,
    pub channels: Option<i32>,
    pub metadata_json: String,
    pub is_single_release: bool,
    /// Loudness normalization (Phase 16).
    pub loudness_lufs: Option<f32>,
    pub loudness_peak: Option<f32>,
    pub album_loudness_lufs: Option<f32>,
}
fn track_dto(t: m::Track) -> DiscTrackDto {
    DiscTrackDto {
        id: t.id.to_string(),
        album_id: t.album_id.to_string(),
        artist_id: t.artist_id.to_string(),
        title: t.title,
        track_no: t.track_no,
        disc_no: t.disc_no,
        duration_ms: t.duration_ms,
        codec: t.codec,
        bitrate_kbps: t.bitrate_kbps,
        file_path: t.file_path,
        file_size: t.file_size,
        sample_rate_hz: t.sample_rate_hz,
        bit_depth: t.bit_depth,
        channels: t.channels,
        metadata_json: t.metadata_json,
        is_single_release: t.is_single_release,
        loudness_lufs: t.loudness_lufs,
        loudness_peak: t.loudness_peak,
        album_loudness_lufs: t.album_loudness_lufs,
    }
}

#[derive(Serialize)]
pub struct DiscoverSectionDto {
    pub id: String,
    pub title: String,
    pub albums: Vec<DiscAlbumDto>,
}

#[derive(Serialize)]
pub struct HomeDto {
    pub sections: Vec<DiscoverSectionDto>,
}

async fn home(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<HomeDto>, ApiError> {
    let sections = s.discover.get_home(&c).await?;
    Ok(Json(HomeDto {
        sections: sections
            .into_iter()
            .map(|sec| DiscoverSectionDto {
                id: sec.id,
                title: sec.title,
                albums: sec.albums.into_iter().map(album_dto).collect(),
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct RadioQuery {
    pub seed_artist_id: Option<String>,
    pub seed_album_id: Option<String>,
    pub seed_track_id: Option<String>,
}

#[derive(Serialize)]
pub struct RadioDto {
    pub tracks: Vec<DiscTrackDto>,
}

fn parse_opt_uuid(s: Option<String>, what: &str) -> Result<Option<Uuid>, ApiError> {
    match s.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => Ok(Some(
            Uuid::parse_str(v)
                .map_err(|_| AppError::InvalidArgument(format!("invalid {what} id")))?,
        )),
        None => Ok(None),
    }
}

async fn radio(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Query(q): Query<RadioQuery>,
) -> Result<Json<RadioDto>, ApiError> {
    let seed_artist = parse_opt_uuid(q.seed_artist_id, "artist")?;
    let seed_album = parse_opt_uuid(q.seed_album_id, "album")?;
    let seed_track = parse_opt_uuid(q.seed_track_id, "track")?;
    let tracks = s
        .discover
        .get_radio(&c, seed_artist, seed_album, seed_track)
        .await?;
    Ok(Json(RadioDto {
        tracks: tracks.into_iter().map(track_dto).collect(),
    }))
}

#[derive(Deserialize)]
pub struct SimilarQuery {
    pub limit: Option<usize>,
}

/// `GET /tracks/:id/similar?limit=` — the "Sounds like this" shelf (Phase 12).
async fn similar(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
    Query(q): Query<SimilarQuery>,
) -> Result<Json<RadioDto>, ApiError> {
    let limit = q.limit.filter(|n| *n > 0).unwrap_or(SIMILAR_DEFAULT);
    let tracks = s.discover.similar_tracks(&c, id, limit).await?;
    Ok(Json(RadioDto {
        tracks: tracks.into_iter().map(track_dto).collect(),
    }))
}

#[derive(Deserialize)]
pub struct RecommendationsBody {
    #[serde(default)]
    pub seed_track_ids: Vec<String>,
    pub limit: Option<usize>,
}

/// `POST /discover/recommendations` — Spotify-style playlist recommendations
/// (Phase 12). Body: `{ seed_track_ids: [...], limit? }`. The client passes the
/// playlist's current track ids; results are based on + exclude them.
async fn recommendations(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Json(body): Json<RecommendationsBody>,
) -> Result<Json<RadioDto>, ApiError> {
    let mut seeds = Vec::with_capacity(body.seed_track_ids.len());
    for raw in &body.seed_track_ids {
        if let Some(id) = parse_opt_uuid(Some(raw.clone()), "seed track")? {
            seeds.push(id);
        }
    }
    let limit = body.limit.filter(|n| *n > 0).unwrap_or(PLAYLIST_REC_DEFAULT);
    let tracks = s.discover.recommend_for_playlist(&c, &seeds, limit).await?;
    Ok(Json(RadioDto {
        tracks: tracks.into_iter().map(track_dto).collect(),
    }))
}

#[derive(Serialize)]
pub struct FingerprintStatusDto {
    pub analyzed: i64,
    pub total: i64,
    pub model_version: String,
    pub enabled: bool,
    /// Tracks with a measured loudness value (Phase 16).
    pub loudness_measured: i64,
    /// Whether loudness normalization is enabled.
    pub loudness_enabled: bool,
}

/// `GET /fingerprint/status` — analysis coverage (any authed user, read-only).
async fn fingerprint_status(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<FingerprintStatusDto>, ApiError> {
    c.require(PermissionLevel::User)?;
    let dto = match &s.fingerprint {
        Some(fp) => {
            let st = fp.status().await;
            FingerprintStatusDto {
                analyzed: st.analyzed,
                total: st.total,
                model_version: st.model_version,
                enabled: true,
                loudness_measured: st.loudness_measured,
                loudness_enabled: st.loudness_enabled,
            }
        }
        None => FingerprintStatusDto {
            analyzed: 0,
            total: 0,
            model_version: String::new(),
            enabled: false,
            loudness_measured: 0,
            loudness_enabled: false,
        },
    };
    Ok(Json(dto))
}

/// `POST /fingerprint/scan` — trigger an analysis pass on demand (Manager+).
async fn fingerprint_scan(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<FingerprintStatusDto>, ApiError> {
    c.require(PermissionLevel::Manager)?;
    let fp = s.fingerprint.as_ref().ok_or_else(|| {
        AppError::InvalidArgument("fingerprinting is disabled (FINGERPRINT_ENABLED off)".into())
    })?;
    fp.run_pass().await;
    let st = fp.status().await;
    Ok(Json(FingerprintStatusDto {
        analyzed: st.analyzed,
        total: st.total,
        model_version: st.model_version,
        enabled: true,
        loudness_measured: st.loudness_measured,
        loudness_enabled: st.loudness_enabled,
    }))
}
