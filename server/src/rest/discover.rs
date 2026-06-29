//! REST discover routes — feature parity with the gRPC `DiscoverService`
//! (Phase 11). `GET /discover` (personalized album shelves) + `GET
//! /discover/radio` (a seeded track queue).

use axum::{
    Extension, Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::error::AppError;
use crate::rest::{ApiError, RestState};

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/discover", get(home))
        .route("/discover/radio", get(radio))
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
    let tracks = s.discover.get_radio(&c, seed_artist, seed_album).await?;
    Ok(Json(RadioDto {
        tracks: tracks.into_iter().map(track_dto).collect(),
    }))
}
