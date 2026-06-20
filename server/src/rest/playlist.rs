//! REST playlist routes — feature parity with `PlaylistService` gRPC.

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::rest::{ApiError, RestState};

pub fn router() -> Router<RestState> {
    Router::new()
        .route("/playlists", post(create_playlist).get(list_my_playlists))
        .route(
            "/playlists/:id",
            get(get_playlist).put(rename_playlist).delete(delete_playlist),
        )
        .route("/playlists/:id/tracks", get(list_tracks).post(add_track))
        .route(
            "/playlists/:id/tracks/:position",
            delete(remove_track_at).put(reorder_track),
        )
        .route("/users/:owner_id/playlists", get(list_for_owner))
}

// ---------------------------------------------------------------------------
// Helpers / DTOs
// ---------------------------------------------------------------------------

fn id(req: &Request<Body>) -> Result<Identity, ApiError> {
    req.extensions()
        .get::<Identity>()
        .cloned()
        .ok_or_else(|| crate::error::AppError::Unauthenticated("missing identity".into()).into())
}

#[derive(Serialize)]
pub struct PlaylistDto {
    pub id: String,
    pub owner_id: String,
    pub name: String,
}
fn playlist_dto(p: m::Playlist) -> PlaylistDto {
    PlaylistDto {
        id: p.id.to_string(),
        owner_id: p.owner_id.to_string(),
        name: p.name,
    }
}

#[derive(Serialize)]
pub struct PlaylistTrackDto {
    pub playlist_id: String,
    pub track_id: String,
    pub position: i32,
}
fn track_dto(t: m::PlaylistTrack) -> PlaylistTrackDto {
    PlaylistTrackDto {
        playlist_id: t.playlist_id.to_string(),
        track_id: t.track_id.to_string(),
        position: t.position,
    }
}

#[derive(Serialize)]
pub struct PlaylistViewDto {
    pub playlist: PlaylistDto,
    pub tracks: Vec<PlaylistTrackDto>,
}

#[derive(Serialize)]
pub struct ListPlaylistsDto {
    pub playlists: Vec<PlaylistDto>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct ListTracksDto {
    pub tracks: Vec<PlaylistTrackDto>,
    pub total: i64,
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreatePlaylistBody {
    pub name: String,
}

async fn create_playlist(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<PlaylistDto>, ApiError> {
    let caller = id(&req)?;
    let body: CreatePlaylistBody = crate::rest::parse_json(req).await?;
    let p = state.playlists.create(&caller, &body.name).await?;
    Ok(Json(playlist_dto(p)))
}

async fn list_my_playlists(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<ListPlaylistsDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.playlists.list_mine(&caller).await?;
    let total = rows.len() as i64;
    Ok(Json(ListPlaylistsDto {
        playlists: rows.into_iter().map(playlist_dto).collect(),
        total,
    }))
}

async fn list_for_owner(
    State(state): State<RestState>,
    Path(owner_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ListPlaylistsDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.playlists.list_for_owner(&caller, owner_id).await?;
    let total = rows.len() as i64;
    Ok(Json(ListPlaylistsDto {
        playlists: rows.into_iter().map(playlist_dto).collect(),
        total,
    }))
}

async fn get_playlist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<PlaylistViewDto>, ApiError> {
    let caller = id(&req)?;
    let view = state.playlists.get_with_tracks(&caller, id_path).await?;
    Ok(Json(PlaylistViewDto {
        playlist: playlist_dto(view.playlist),
        tracks: view.tracks.into_iter().map(track_dto).collect(),
    }))
}

#[derive(Deserialize)]
pub struct RenamePlaylistBody {
    pub name: String,
}

async fn rename_playlist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<PlaylistDto>, ApiError> {
    let caller = id(&req)?;
    let body: RenamePlaylistBody = crate::rest::parse_json(req).await?;
    let p = state.playlists.rename(&caller, id_path, &body.name).await?;
    Ok(Json(playlist_dto(p)))
}

async fn delete_playlist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let deleted = state.playlists.delete(&caller, id_path).await?;
    Ok(if deleted {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

// ---------------------------------------------------------------------------
// Track ops
// ---------------------------------------------------------------------------

async fn list_tracks(
    State(state): State<RestState>,
    Path(playlist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ListTracksDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.playlists.list_tracks(&caller, playlist_id).await?;
    let total = rows.len() as i64;
    Ok(Json(ListTracksDto {
        tracks: rows.into_iter().map(track_dto).collect(),
        total,
    }))
}

#[derive(Deserialize)]
pub struct AddTrackBody {
    pub track_id: Uuid,
    /// 1-based position. `None`/`0` = append.
    pub position: Option<i32>,
}

async fn add_track(
    State(state): State<RestState>,
    Path(playlist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<PlaylistTrackDto>, ApiError> {
    let caller = id(&req)?;
    let body: AddTrackBody = crate::rest::parse_json(req).await?;
    let row = match body.position {
        Some(p) if p > 0 => {
            state
                .playlists
                .insert_track(&caller, playlist_id, body.track_id, p)
                .await?
        }
        _ => {
            state
                .playlists
                .add_track(&caller, playlist_id, body.track_id)
                .await?
        }
    };
    Ok(Json(track_dto(row)))
}

async fn remove_track_at(
    State(state): State<RestState>,
    Path((playlist_id, position)): Path<(Uuid, i32)>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let removed = state
        .playlists
        .remove_track_at(&caller, playlist_id, position)
        .await?;
    Ok(if removed.is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}

#[derive(Deserialize)]
pub struct ReorderBody {
    pub to: i32,
}

async fn reorder_track(
    State(state): State<RestState>,
    Path((playlist_id, from_position)): Path<(Uuid, i32)>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let body: ReorderBody = crate::rest::parse_json(req).await?;
    state
        .playlists
        .reorder(&caller, playlist_id, from_position, body.to)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
