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
        .route("/artists/:id/image", get(serve_artist_image).post(upload_artist_image))
        .route("/artists/:id/merge", post(merge_artists))
        .route("/artists/:id/library-paths", get(list_artist_library_paths))
        .route("/artists/:id/library-language", post(set_artist_language))
        .route("/artists/:id/aliases", get(list_artist_aliases).post(add_artist_alias))
        .route("/artists/:id/aliases/:alias_id", delete(remove_artist_alias))
        .route("/artists/:id/primary-alias", put(set_primary_artist_alias))
        // Albums
        .route("/albums", post(create_album))
        .route("/albums/search", get(search_albums))
        .route("/albums/:id", get(get_album).put(update_album).delete(delete_album))
        .route("/albums/:id/type", post(set_album_type))
        .route("/albums/:id/tracks", get(list_tracks_by_album))
        .route("/albums/:id/cover", get(serve_album_cover).post(upload_album_cover))
        .route("/albums/:id/artwork", post(fetch_album_artwork))
        .route("/albums/:id/merge", post(merge_albums))
        .route("/albums/:id/aliases", get(list_album_aliases).post(add_album_alias))
        .route("/albums/:id/aliases/:alias_id", delete(remove_album_alias))
        .route("/albums/:id/primary-alias", put(set_primary_album_alias))
        // Tracks
        .route("/tracks", post(create_track))
        .route("/tracks/search", get(search_tracks))
        .route("/tracks/:id", get(get_track).put(update_track).delete(delete_track))
        .route("/tracks/:id/metadata", patch(edit_track_metadata))
        .route("/tracks/:id/move", post(move_track))
        .route("/tracks/:id/single-release", post(set_track_single_release))
        // Storage breakdown (homepage widget)
        .route("/library/storage", get(get_library_storage))
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

/// One known spelling of an artist/album. `name` is the spelling (artist name
/// or album title); `sort_name` is artist-only (`None` for albums).
#[derive(Serialize)]
pub struct AliasDto {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub language: Option<String>,
    pub is_primary: bool,
}
fn artist_alias_dto(a: m::ArtistAlias) -> AliasDto {
    AliasDto {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name,
        language: a.language,
        is_primary: a.is_primary,
    }
}
fn album_alias_dto(a: m::AlbumAlias) -> AliasDto {
    AliasDto {
        id: a.id.to_string(),
        name: a.title,
        sort_name: None,
        language: a.language,
        is_primary: a.is_primary,
    }
}

#[derive(Serialize)]
pub struct ArtistDto {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub image_path: Option<String>,
    /// Every known spelling. Populated on single-entity reads/mutations only.
    pub aliases: Vec<AliasDto>,
    /// Sum of the on-disk bytes of every track owned by this artist.
    pub storage_bytes: i64,
}
fn artist_dto(a: m::Artist) -> ArtistDto {
    ArtistDto {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name,
        image_path: a.image_path,
        aliases: Vec::new(),
        storage_bytes: a.storage_bytes,
    }
}

#[derive(Serialize)]
pub struct AlbumDto {
    pub id: String,
    pub artist_id: String,
    pub title: String,
    pub release_year: Option<i32>,
    /// Classification: `album` | `ep` | `single`.
    pub album_type: String,
    pub cover_path: Option<String>,
    pub aliases: Vec<AliasDto>,
    /// Sum of the on-disk bytes of every track on this album.
    pub storage_bytes: i64,
}
fn album_dto(a: m::Album) -> AlbumDto {
    AlbumDto {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year,
        album_type: a.album_type,
        cover_path: a.cover_path,
        aliases: Vec::new(),
        storage_bytes: a.storage_bytes,
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
    pub sample_rate_hz: Option<i32>,
    pub bit_depth: Option<i32>,
    pub channels: Option<i32>,
    pub metadata_json: String,
    pub is_single_release: bool,
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
        sample_rate_hz: t.sample_rate_hz,
        bit_depth: t.bit_depth,
        channels: t.channels,
        metadata_json: t.metadata_json,
        is_single_release: t.is_single_release,
    }
}

/// Build an `ArtistDto` with its alias list populated (single-entity paths).
async fn artist_dto_full(
    state: &RestState,
    caller: &Identity,
    a: m::Artist,
) -> Result<ArtistDto, ApiError> {
    let aliases = state.library.list_artist_aliases(caller, a.id).await?;
    let mut dto = artist_dto(a);
    dto.aliases = aliases.into_iter().map(artist_alias_dto).collect();
    Ok(dto)
}

/// Build an `AlbumDto` with its alias list populated (single-entity paths).
async fn album_dto_full(
    state: &RestState,
    caller: &Identity,
    a: m::Album,
) -> Result<AlbumDto, ApiError> {
    let aliases = state.library.list_album_aliases(caller, a.id).await?;
    let mut dto = album_dto(a);
    dto.aliases = aliases.into_iter().map(album_alias_dto).collect();
    Ok(dto)
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
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
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
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
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
    Ok(Json(album_dto_full(&state, &caller, a).await?))
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
    Ok(Json(album_dto_full(&state, &caller, a).await?))
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
        sample_rate_hz: None,
        bit_depth: None,
        channels: None,
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
    let artwork = state
        .artwork
        .as_ref()
        .filter(|a| a.auto_fetch_enabled())
        .ok_or_else(|| {
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
    let caller = id(&req)?;
    let album = state.library.get_album(&caller, album_id).await?;

    let cover_path = match album.cover_path {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(&p),
        // No cover_path set → optionally try to fetch if auto-fetch is on.
        _ => {
            if let Some(artwork) = state.artwork.as_ref().filter(|a| a.auto_fetch_enabled()) {
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

    // Serve the optimized (downscaled) variant — the tiny low-res placeholder
    // when `?lowres=1`, else the full optimized image — generating it on demand.
    // Falls back to the original on any failure.
    let variant = lowres_variant(&req);
    let serve_path = match &state.optimizer {
        Some(o) => {
            o.ensure_optimized(&crate::services::ImageOptimizer::album_key(album_id), &cover_path, variant)
                .await
        }
        None => cover_path,
    };
    image_file_response(&serve_path).await
}

// ---------------------------------------------------------------------------
// Merge + aliases (Phase 10; Manager+ gated, audited)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct MergeBody {
    pub duplicate_id: Uuid,
}

async fn merge_artists(
    State(state): State<RestState>,
    Path(survivor_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let body: MergeBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .merge_artists(&caller, survivor_id, body.duplicate_id)
        .await?;
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
}

async fn list_artist_library_paths(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<crate::services::library::ArtistStoragePaths>, ApiError> {
    let caller = id(&req)?;
    let paths = state
        .library
        .list_artist_library_paths(&caller, artist_id)
        .await?;
    Ok(Json(paths))
}

#[derive(Deserialize)]
pub struct SetLanguageBody {
    pub target_language: String,
    #[serde(default)]
    pub target_folder: Option<String>,
}

async fn set_artist_language(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<crate::services::library::RelocateReport>, ApiError> {
    let caller = id(&req)?;
    let body: SetLanguageBody = crate::rest::parse_json(req).await?;
    let report = state
        .library
        .set_artist_language(
            &caller,
            artist_id,
            &body.target_language,
            body.target_folder.as_deref(),
        )
        .await?;
    Ok(Json(report))
}

async fn merge_albums(
    State(state): State<RestState>,
    Path(survivor_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: MergeBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .merge_albums(&caller, survivor_id, body.duplicate_id)
        .await?;
    Ok(Json(album_dto_full(&state, &caller, a).await?))
}

#[derive(Deserialize)]
pub struct MoveTrackBody {
    pub album_id: Uuid,
    #[serde(default)]
    pub single_release: bool,
}

async fn move_track(
    State(state): State<RestState>,
    Path(track_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let body: MoveTrackBody = crate::rest::parse_json(req).await?;
    let t = state
        .library
        .move_track(&caller, track_id, body.album_id, body.single_release)
        .await?;
    Ok(Json(track_dto(t)))
}

#[derive(Deserialize)]
pub struct SingleReleaseBody {
    pub single_release: bool,
}

async fn set_track_single_release(
    State(state): State<RestState>,
    Path(track_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<TrackDto>, ApiError> {
    let caller = id(&req)?;
    let body: SingleReleaseBody = crate::rest::parse_json(req).await?;
    let t = state
        .library
        .set_track_single_release(&caller, track_id, body.single_release)
        .await?;
    Ok(Json(track_dto(t)))
}

#[derive(Deserialize)]
pub struct AlbumTypeBody {
    pub album_type: String,
    /// Optional main single to flag before the single-song invariant is checked.
    #[serde(default)]
    pub single_track_id: Option<Uuid>,
}

async fn set_album_type(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: AlbumTypeBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .set_album_type(&caller, album_id, &body.album_type, body.single_track_id)
        .await?;
    Ok(Json(album_dto_full(&state, &caller, a).await?))
}

#[derive(Deserialize)]
pub struct AddArtistAliasBody {
    pub name: String,
    pub sort_name: Option<String>,
    pub language: Option<String>,
}

async fn list_artist_aliases(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<Vec<AliasDto>>, ApiError> {
    let caller = id(&req)?;
    let rows = state.library.list_artist_aliases(&caller, artist_id).await?;
    Ok(Json(rows.into_iter().map(artist_alias_dto).collect()))
}

async fn add_artist_alias(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let body: AddArtistAliasBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .add_artist_alias(
            &caller,
            artist_id,
            &body.name,
            body.sort_name.as_deref(),
            body.language.as_deref(),
        )
        .await?;
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
}

async fn remove_artist_alias(
    State(state): State<RestState>,
    Path((artist_id, alias_id)): Path<(Uuid, Uuid)>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let a = state
        .library
        .remove_artist_alias(&caller, artist_id, alias_id)
        .await?;
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
}

#[derive(Deserialize)]
pub struct SetPrimaryAliasBody {
    pub alias_id: Uuid,
}

async fn set_primary_artist_alias(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let caller = id(&req)?;
    let body: SetPrimaryAliasBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .set_primary_artist_alias(&caller, artist_id, body.alias_id)
        .await?;
    Ok(Json(artist_dto_full(&state, &caller, a).await?))
}

#[derive(Deserialize)]
pub struct AddAlbumAliasBody {
    pub title: String,
    pub language: Option<String>,
}

async fn list_album_aliases(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<Vec<AliasDto>>, ApiError> {
    let caller = id(&req)?;
    let rows = state.library.list_album_aliases(&caller, album_id).await?;
    Ok(Json(rows.into_iter().map(album_alias_dto).collect()))
}

async fn add_album_alias(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: AddAlbumAliasBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .add_album_alias(&caller, album_id, &body.title, body.language.as_deref())
        .await?;
    Ok(Json(album_dto_full(&state, &caller, a).await?))
}

async fn remove_album_alias(
    State(state): State<RestState>,
    Path((album_id, alias_id)): Path<(Uuid, Uuid)>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let a = state
        .library
        .remove_album_alias(&caller, album_id, alias_id)
        .await?;
    Ok(Json(album_dto_full(&state, &caller, a).await?))
}

async fn set_primary_album_alias(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let caller = id(&req)?;
    let body: SetPrimaryAliasBody = crate::rest::parse_json(req).await?;
    let a = state
        .library
        .set_primary_album_alias(&caller, album_id, body.alias_id)
        .await?;
    Ok(Json(album_dto_full(&state, &caller, a).await?))
}

// ---------------------------------------------------------------------------
// Image upload (Phase 9 metadata-editing extension; Manager+ gated)
//
// Raw-body upload (image/* + bytes), mirroring the REST-only cover *serving*
// above — binary blobs go over REST, not gRPC. Album covers reuse
// `ArtworkService::set_cover_from_bytes` (cache + audited `cover_path` update
// + embed into track files); artist images use `set_artist_image_from_bytes`.
// ---------------------------------------------------------------------------

/// Max bytes accepted for an uploaded cover / artist image (raw body).
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Pull the caller + an image body off the request, validating the
/// `Content-Type` is `image/*` and the body is non-empty + within the cap.
async fn read_image_body(
    req: Request<Body>,
) -> Result<(Identity, crate::services::artwork::CoverImage), ApiError> {
    let caller = id(&req)?;
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !content_type.starts_with("image/") {
        return Err(crate::error::AppError::InvalidArgument(format!(
            "expected an image/* Content-Type for upload, got {content_type:?}"
        ))
        .into());
    }
    let bytes = axum::body::to_bytes(req.into_body(), MAX_IMAGE_BYTES)
        .await
        .map_err(|_| {
            crate::error::AppError::InvalidArgument(format!(
                "image body too large (max {} MiB) or unreadable",
                MAX_IMAGE_BYTES / (1024 * 1024)
            ))
        })?
        .to_vec();
    if bytes.is_empty() {
        return Err(crate::error::AppError::InvalidArgument("empty image body".into()).into());
    }
    Ok((caller, crate::services::artwork::CoverImage { bytes, content_type }))
}

fn require_artwork(state: &RestState) -> Result<&crate::services::ArtworkService, ApiError> {
    state.artwork.as_ref().ok_or_else(|| {
        ApiError::from(crate::error::AppError::Config(
            "artwork storage not configured (set ARTWORK_PATH)".into(),
        ))
    })
}

/// `POST /albums/:id/cover` — upload a cover image (raw `image/*` body).
async fn upload_album_cover(
    State(state): State<RestState>,
    Path(album_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<AlbumDto>, ApiError> {
    let (caller, image) = read_image_body(req).await?;
    let artwork = require_artwork(&state)?;
    let cover_path = artwork.set_cover_from_bytes(&caller, album_id, &image).await?;
    warm_optimized(&state, crate::services::ImageOptimizer::album_key(album_id), cover_path);
    let album = state.library.get_album(&caller, album_id).await?;
    Ok(Json(album_dto(album)))
}

/// `POST /artists/:id/image` — upload an artist image (raw `image/*` body).
async fn upload_artist_image(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<ArtistDto>, ApiError> {
    let (caller, image) = read_image_body(req).await?;
    let artwork = require_artwork(&state)?;
    let image_path = artwork
        .set_artist_image_from_bytes(&caller, artist_id, &image)
        .await?;
    warm_optimized(&state, crate::services::ImageOptimizer::artist_key(artist_id), image_path);
    let artist = state.library.get_artist(&caller, artist_id).await?;
    Ok(Json(artist_dto(artist)))
}

/// `GET /artists/:id/image` — serve the artist's cached (optimized) image, or 404.
async fn serve_artist_image(
    State(state): State<RestState>,
    Path(artist_id): Path<Uuid>,
    req: Request<Body>,
) -> Result<(axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>), ApiError> {
    let caller = id(&req)?;
    let artist = state.library.get_artist(&caller, artist_id).await?;

    let image_path = match artist.image_path {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(&p),
        _ => {
            return Err(ApiError::from(crate::error::AppError::NotFound(
                "no image available for this artist".into(),
            )));
        }
    };
    if !image_path.is_file() {
        return Err(ApiError::from(crate::error::AppError::NotFound(
            "artist image file not found".into(),
        )));
    }

    let variant = lowres_variant(&req);
    let serve_path = match &state.optimizer {
        Some(o) => {
            o.ensure_optimized(&crate::services::ImageOptimizer::artist_key(artist_id), &image_path, variant)
                .await
        }
        None => image_path,
    };
    image_file_response(&serve_path).await
}

/// `?lowres=1` (or bare `?lowres`) selects the tiny low-res placeholder variant.
fn lowres_variant(req: &Request<Body>) -> crate::services::Variant {
    let low = req
        .uri()
        .query()
        .map(|q| q.split('&').any(|p| p == "lowres=1" || p == "lowres"))
        .unwrap_or(false);
    if low {
        crate::services::Variant::Low
    } else {
        crate::services::Variant::Full
    }
}

/// Read `path` and build a 200 image response, deriving the content-type from
/// the file extension. Shared by the cover + artist serve handlers.
async fn image_file_response(
    path: &std::path::Path,
) -> Result<(axum::http::StatusCode, axum::http::HeaderMap, Vec<u8>), ApiError> {
    use axum::http::header::{CONTENT_TYPE, HeaderValue};
    let bytes = tokio::fs::read(path).await.map_err(crate::error::AppError::Io)?;
    let content_type = match path
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

/// Warm the optimized cache for a just-uploaded image, in the background so the
/// upload response returns immediately. Best-effort: if it loses the race the
/// next serve generates it on demand anyway.
fn warm_optimized(state: &RestState, key: String, source: String) {
    if let Some(opt) = state.optimizer.clone() {
        tokio::spawn(async move {
            opt.ensure_all(&key, std::path::Path::new(&source)).await;
        });
    }
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

#[derive(Serialize)]
pub struct LibraryStorageDto {
    pub music_bytes: i64,
    pub podcast_bytes: i64,
    pub artwork_bytes: i64,
    pub other_bytes: i64,
    pub total_bytes: i64,
    pub track_count: i64,
    pub album_count: i64,
    pub artist_count: i64,
    pub podcast_count: i64,
    pub episode_count: i64,
    pub computed_at: String,
}

/// `GET /library/storage` — the homepage storage breakdown. Any authed user.
async fn get_library_storage(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<LibraryStorageDto>, ApiError> {
    // Resolving the caller enforces a valid token (read open to any user).
    let _caller = id(&req)?;
    let s = state.storage.get_stats().await?;
    Ok(Json(LibraryStorageDto {
        music_bytes: s.music_bytes,
        podcast_bytes: s.podcast_bytes,
        artwork_bytes: s.artwork_bytes,
        other_bytes: s.other_bytes,
        total_bytes: s.total_bytes,
        track_count: s.track_count,
        album_count: s.album_count,
        artist_count: s.artist_count,
        podcast_count: s.podcast_count,
        episode_count: s.episode_count,
        computed_at: crate::time_fmt::rfc3339(s.computed_at),
    }))
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
