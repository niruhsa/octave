//! REST library routes — feature parity with `LibraryService` gRPC.

use std::path::PathBuf;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    routing::{delete, get, patch, post, put},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, NewTrack};
use crate::rest::{ApiError, RestState};

pub fn router() -> Router<RestState> {
    Router::new()
        // Artists
        .route("/artists", post(create_artist).get(list_artists))
        .route("/artists/search", get(search_artists))
        .route("/artists/:id", get(get_artist).put(update_artist).delete(delete_artist))
        .route("/artists/:id/albums", get(list_albums_by_artist))
        // Albums
        .route("/albums", post(create_album))
        .route("/albums/search", get(search_albums))
        .route("/albums/:id", get(get_album).put(update_album).delete(delete_album))
        .route("/albums/:id/tracks", get(list_tracks_by_album))
        .route("/albums/:id/cover", get(serve_album_cover))
        .route("/albums/:id/artwork", post(fetch_album_artwork))
        // Tracks
        .route("/tracks", post(create_track))
        .route("/tracks/search", get(search_tracks))
        .route("/tracks/:id", get(get_track).put(update_track).delete(delete_track))
        .route("/tracks/:id/metadata", patch(edit_track_metadata))
        // Scan
        .route("/library/scan", post(scan_library))
        .route("/library/rescan", post(rescan_library))
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

#[derive(Deserialize, Default)]
pub struct Page {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Deserialize)]
pub struct SearchQ {
    pub q: String,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Serialize)]
pub struct ArtistDto {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
}
fn artist_dto(a: m::Artist) -> ArtistDto {
    ArtistDto {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name,
    }
}

#[derive(Serialize)]
pub struct AlbumDto {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
}
fn album_dto(a: m::Album) -> AlbumDto {
    AlbumDto {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year,
        cover_path: a.cover_path,
    }
}

#[derive(Serialize)]
pub struct TrackDto {
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
    pub metadata_json: String,
}
fn track_dto(t: m::Track) -> TrackDto {
    TrackDto {
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
        metadata_json: t.metadata_json,
    }
}

// ---------------------------------------------------------------------------
// Artists
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateArtistBody {
    pub name: String,
    pub sort_name: Option<String>,
}

async fn create_artist(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let body: CreateArtistBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .create_artist(&caller, &body.name, body.sort_name.as_deref())
        .await?;
    Ok(Json(artist_dto(a)))
}

#[derive(Serialize)]
pub struct ListArtistsDto {
    pub artists: Vec<ArtistDto>,
    pub total: i64,
}

async fn list_artists(
    State(state): State<RestState>,
    Query(p): Query<Page>,
    req: Request<Body>,
) -> Result<Json<ListArtistsDto>, ApiError> {
    let caller = id(&req)?;
    let (rows, total) = state.library.list_artists(&caller, p.limit, p.offset).await?;
    Ok(Json(ListArtistsDto {
        artists: rows.into_iter().map(artist_dto).collect(),
        total,
    }))
}

async fn search_artists(
    State(state): State<RestState>,
    Query(q): Query<SearchQ>,
    req: Request<Body>,
) -> Result<Json<Vec<ArtistDto>>, ApiError> {
    let caller = id(&req)?;
    let rows = state
        .library
        .search_artists(&caller, &q.q, q.limit, q.offset)
        .await?;
    Ok(Json(rows.into_iter().map(artist_dto).collect()))
}

async fn get_artist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let a = state.library.get_artist(&caller, id_path).await?;
    Ok(Json(artist_dto(a)))
}

async fn update_artist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let body: CreateArtistBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .update_artist(&caller, id_path, &body.name, body.sort_name.as_deref())
        .await?;
    Ok(Json(artist_dto(a)))
}

async fn delete_artist(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let deleted = state.library.delete_artist(&caller, id_path).await?;
    Ok(if deleted { StatusCode::NO_CONTENT } else { StatusCode::NOT_FOUND })
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateAlbumBody {
    pub artist_id: Uuid,
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
}
#[derive(Deserialize)]
pub struct UpdateAlbumBody {
    pub title: String,
    pub release_year: Option<i32>,
    pub cover_path: Option<String>,
}

async fn create_album(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: CreateAlbumBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .create_album(
            &caller,
            body.artist_id,
            &body.title,
            body.release_year,
            body.cover_path.as_deref(),
        )
        .await?;
    Ok(Json(album_dto(a)))
}

#[derive(Serialize)]
pub struct ListAlbumsDto {
    pub albums: Vec<AlbumDto>,
    pub total: i64,
}

async fn list_albums_by_artist(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ListAlbumsDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.library.list_albums_by_artist(&caller, artist_id).await?;
    let total = rows.len() as i64;
    Ok(Json(ListAlbumsDto {
        albums: rows.into_iter().map(album_dto).collect(),
        total,
    }))
}

async fn search_albums(
    State(state): State<RestState>,
    Query(q): Query<SearchQ>,
    req: Request<Body>,
) -> Result<Json<Vec<AlbumDto>>, ApiError> {
    let caller = id(&req)?;
    let rows = state
        .library
        .search_albums(&caller, &q.q, q.limit, q.offset)
        .await?;
    Ok(Json(rows.into_iter().map(album_dto).collect()))
}

async fn get_album(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let a = state.library.get_album(&caller, id_path).await?;
    Ok(Json(album_dto(a)))
}

async fn update_album(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: UpdateAlbumBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .update_album(
            &caller,
            id_path,
            &body.title,
            body.release_year,
            body.cover_path.as_deref(),
        )
        .await?;
    Ok(Json(album_dto(a)))
}

async fn delete_album(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let deleted = state.library.delete_album(&caller, id_path).await?;
    Ok(if deleted { StatusCode::NO_CONTENT } else { StatusCode::NOT_FOUND })
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateTrackBody {
    pub album_id: Uuid,
    pub artist_id: Uuid,
    pub title: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub duration_ms: i64,
    pub codec: String,
    pub bitrate_kbps: Option<i32>,
    pub file_path: String,
    pub file_size: Option<i64>,
    pub metadata_json: Option<String>,
}
#[derive(Deserialize)]
pub struct UpdateTrackBody {
    pub title: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub metadata_json: Option<String>,
}

async fn create_track(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let b: CreateTrackBody = crate::rest::parse_json(req).await?;
    let new = NewTrack {
        album_id: b.album_id,
        artist_id: b.artist_id,
        title: b.title,
        track_no: b.track_no,
        disc_no: b.disc_no,
        duration_ms: b.duration_ms,
        codec: b.codec,
        bitrate_kbps: b.bitrate_kbps,
        file_path: b.file_path,
        file_size: b.file_size,
        metadata_json: b.metadata_json.unwrap_or_else(|| "{}".to_string()),
    };
    let t = state.library.create_track(&caller, new).await?;
    Ok(Json(track_dto(t)))
}

async fn get_track(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let t = state.library.get_track(&caller, id_path).await?;
    Ok(Json(track_dto(t)))
}

async fn update_track(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let b: UpdateTrackBody = crate::rest::parse_json(req).await?;
    let meta = b.metadata_json.unwrap_or_else(|| "{}".to_string());
    let t = state
        .library
        .update_track(&caller, id_path, &b.title, b.track_no, b.disc_no, &meta)
        .await?;
    Ok(Json(track_dto(t)))
}

async fn delete_track(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = id(&req)?;
    let deleted = state.library.delete_track(&caller, id_path).await?;
    Ok(if deleted { StatusCode::NO_CONTENT } else { StatusCode::NOT_FOUND })
}

#[derive(Serialize)]
pub struct ListTracksDto {
    pub tracks: Vec<TrackDto>,
    pub total: i64,
}

async fn list_tracks_by_album(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ListTracksDto>, ApiError> {
    let caller = id(&req)?;
    let rows = state.library.list_tracks_by_album(&caller, album_id).await?;
    let total = rows.len() as i64;
    Ok(Json(ListTracksDto {
        tracks: rows.into_iter().map(track_dto).collect(),
        total,
    }))
}

async fn search_tracks(
    State(state): State<RestState>,
    Query(q): Query<SearchQ>,
    req: Request<Body>,
) -> Result<Json<Vec<TrackDto>>, ApiError> {
    let caller = id(&req)?;
    let rows = state
        .library
        .search_tracks(&caller, &q.q, q.limit, q.offset)
        .await?;
    Ok(Json(rows.into_iter().map(track_dto).collect()))
}

// ---------------------------------------------------------------------------
// Metadata & Artwork (Phase 7)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EditMetadataBody {
    pub title: Option<String>,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub metadata_json: Option<String>,
    pub year: Option<i32>,
}

async fn edit_track_metadata(
    State(state): State<RestState>,
    Path(id_path): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let b: EditMetadataBody = crate::rest::parse_json(req).await?;
    let edit = crate::services::MetadataEdit {
        title: b.title,
        track_no: b.track_no,
        disc_no: b.disc_no,
        metadata_json: b.metadata_json,
        year: b.year,
    };
    let t = state.metadata.edit_track(&caller, id_path, edit).await?;
    Ok(Json(track_dto(t)))
}

#[derive(Serialize)]
pub struct ArtworkDto {
    pub found: bool,
    pub cover_path: Option<String>,
}

async fn fetch_album_artwork(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtworkDto>, ApiError> {
    let caller = id(&req)?;
    let artwork = state.artwork.as_ref().ok_or_else(|| {
        ApiError::from(crate::error::AppError::Config(
            "artwork fetch is disabled (set FETCH_ARTWORK)".into(),
        ))
    })?;
    let cover = artwork.fetch_for_album(&caller, album_id).await?;
    Ok(Json(ArtworkDto {
        found: cover.is_some(),
        cover_path: cover,
    }))
}

/// Serve the cached cover image for an album (if any).
///
/// Returns the image bytes with the correct `Content-Type` derived from the
/// cached file extension, or 404 when no cover has been fetched yet / the
/// cached file is missing.
async fn serve_album_cover(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<(axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>), ApiError> {
    use axum::http::header::{CONTENT_TYPE, HeaderValue};

    let caller = id(&req)?;
    let album = state.library.get_album(&caller, album_id).await?;

    let cover_path = match album.cover_path {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(&p),
        // No cover_path set → optionally try to fetch if the artwork
        // service is configured.
        _ => {
            if let Some(artwork) = &state.artwork {
                match artwork.fetch_for_album(&caller, album_id).await {
                    Ok(Some(new_path)) => std::path::PathBuf::from(&new_path),
                    Ok(None) => return Err(ApiError::from(crate::error::AppError::NotFound(
                        "no cover art available for this album".into(),
                    ))),
                    Err(e) => return Err(ApiError::from(e)),
                }
            } else {
                return Err(ApiError::from(crate::error::AppError::NotFound(
                    "no cover art available for this album".into(),
                )));
            }
        }
    };

    if !cover_path.is_file() {
        return Err(ApiError::from(crate::error::AppError::NotFound(
            "cached cover file not found".into(),
        )));
    }

    let bytes = tokio::fs::read(&cover_path).await
        .map_err(|e| crate::error::AppError::Io(e))?;

    let content_type = match cover_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    Ok((axum::http::StatusCode::OK, headers, bytes))
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ScanBody {
    pub root_path: Option<String>,
}
#[derive(Serialize)]
pub struct ScanDto {
    pub tracks_added: u64,
    pub tracks_skipped: u64,
    pub errors: u64,
}

async fn scan_library(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<ScanDto>, ApiError> {
    let caller = id(&req)?;
    let body: ScanBody = crate::rest::parse_json(req).await?;
    let root = body.root_path.map(PathBuf::from);
    let report = state.scan.scan(&caller, root.as_deref()).await?;
    Ok(Json(ScanDto {
        tracks_added: report.tracks_added,
        tracks_skipped: report.tracks_skipped,
        errors: report.errors,
    }))
}

#[derive(Serialize)]
pub struct RescanDto {
    pub tracks_checked: u64,
    pub tracks_updated: u64,
    pub errors: u64,
}

async fn rescan_library(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<RescanDto>, ApiError> {
    let caller = id(&req)?;
    let report = state.scan.refresh_durations(&caller).await?;
    Ok(Json(RescanDto {
        tracks_checked: report.total,
        tracks_updated: report.corrected,
        errors: report.errors,
    }))
}

// Tell the unused-import linter we use `delete`/`put` indirectly via the macro DSL.
#[allow(dead_code)]
fn _route_keepalive() {
    let _: Router<RestState> = Router::new()
        .route("/__noop__", delete(noop_h))
        .route("/__noop2__", put(noop_h));
}
async fn noop_h() -> StatusCode { StatusCode::NO_CONTENT }
