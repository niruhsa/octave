//! REST favorites routes — feature parity with the gRPC `FavoriteService`
//! (Phase 11). Per-entity toggle routes mirror the follow routes
//! (`/{tracks,albums,artists}/:id/favorite`), plus list reads under
//! `/favorites/...`.

use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, FavoriteKind};
use crate::rest::{ApiError, RestState};

pub fn router() -> Router<RestState> {
    Router::new()
        .route(
            "/tracks/:id/favorite",
            get(is_fav_track).post(fav_track).delete(unfav_track),
        )
        .route(
            "/albums/:id/favorite",
            get(is_fav_album).post(fav_album).delete(unfav_album),
        )
        .route(
            "/artists/:id/favorite",
            get(is_fav_artist).post(fav_artist).delete(unfav_artist),
        )
        .route("/favorites/tracks", get(list_tracks))
        .route("/favorites/albums", get(list_albums))
        .route("/favorites/artists", get(list_artists))
        .route("/favorites/track-ids", get(list_track_ids))
}

#[derive(Serialize)]
pub struct FavoriteStatusDto {
    pub favorited: bool,
}

#[derive(Serialize)]
pub struct FavArtistDto {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub image_path: Option<String>,
    pub storage_bytes: i64,
}
fn artist_dto(a: m::Artist) -> FavArtistDto {
    FavArtistDto {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name,
        image_path: a.image_path,
        storage_bytes: a.storage_bytes,
    }
}

#[derive(Serialize)]
pub struct FavAlbumDto {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
    pub storage_bytes: i64,
}
fn album_dto(a: m::Album) -> FavAlbumDto {
    FavAlbumDto {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year,
        cover_path: a.cover_path,
        storage_bytes: a.storage_bytes,
    }
}

#[derive(Serialize)]
pub struct FavTrackDto {
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
fn track_dto(t: m::Track) -> FavTrackDto {
    FavTrackDto {
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

// --- per-kind toggle handlers ---
//
// `Identity` is read via `Extension` (the auth middleware inserts it into the
// request extensions), so these don't consume the whole `Request` body.

async fn fav_track(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.favorite(&c, FavoriteKind::Track, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: true }))
}
async fn unfav_track(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.unfavorite(&c, FavoriteKind::Track, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: false }))
}
async fn is_fav_track(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    let favorited = s.favorites.is_favorite(&c, FavoriteKind::Track, id).await?;
    Ok(Json(FavoriteStatusDto { favorited }))
}

async fn fav_album(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.favorite(&c, FavoriteKind::Album, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: true }))
}
async fn unfav_album(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.unfavorite(&c, FavoriteKind::Album, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: false }))
}
async fn is_fav_album(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    let favorited = s.favorites.is_favorite(&c, FavoriteKind::Album, id).await?;
    Ok(Json(FavoriteStatusDto { favorited }))
}

async fn fav_artist(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.favorite(&c, FavoriteKind::Artist, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: true }))
}
async fn unfav_artist(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    s.favorites.unfavorite(&c, FavoriteKind::Artist, id).await?;
    Ok(Json(FavoriteStatusDto { favorited: false }))
}
async fn is_fav_artist(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
    Path(id): Path<Uuid>,
) -> Result<Json<FavoriteStatusDto>, ApiError> {
    let favorited = s
        .favorites
        .is_favorite(&c, FavoriteKind::Artist, id)
        .await?;
    Ok(Json(FavoriteStatusDto { favorited }))
}

// --- list handlers ---

#[derive(Serialize)]
pub struct ListTracksDto {
    pub tracks: Vec<FavTrackDto>,
}
async fn list_tracks(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<ListTracksDto>, ApiError> {
    let rows = s.favorites.list_tracks(&c).await?;
    Ok(Json(ListTracksDto {
        tracks: rows.into_iter().map(track_dto).collect(),
    }))
}

#[derive(Serialize)]
pub struct ListAlbumsDto {
    pub albums: Vec<FavAlbumDto>,
}
async fn list_albums(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<ListAlbumsDto>, ApiError> {
    let rows = s.favorites.list_albums(&c).await?;
    Ok(Json(ListAlbumsDto {
        albums: rows.into_iter().map(album_dto).collect(),
    }))
}

#[derive(Serialize)]
pub struct ListArtistsDto {
    pub artists: Vec<FavArtistDto>,
}
async fn list_artists(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<ListArtistsDto>, ApiError> {
    let rows = s.favorites.list_artists(&c).await?;
    Ok(Json(ListArtistsDto {
        artists: rows.into_iter().map(artist_dto).collect(),
    }))
}

#[derive(Serialize)]
pub struct ListTrackIdsDto {
    pub track_ids: Vec<String>,
}
async fn list_track_ids(
    State(s): State<RestState>,
    Extension(c): Extension<Identity>,
) -> Result<Json<ListTrackIdsDto>, ApiError> {
    let ids = s.favorites.favorited_track_ids(&c).await?;
    Ok(Json(ListTrackIdsDto {
        track_ids: ids.into_iter().map(|i| i.to_string()).collect(),
    }))
}
