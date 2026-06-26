//! REST podcast routes — feature parity with the gRPC `PodcastService`.
//!
//! Episodes stream through the same byte-range helper as tracks
//! ([`crate::rest::streaming::serve_resolved`]). The whole router 404s when
//! podcasts aren't enabled (no `PODCAST_PATH`).

use std::path::Path as FsPath;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    http::header::CONTENT_TYPE,
    response::Response,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models as m;
use crate::error::AppError;
use crate::rest::{ApiError, RestState};
use crate::services::PodcastService;
use crate::time_fmt::rfc3339;

pub fn router() -> Router<RestState> {
    Router::new()
        // Discovery + catalog
        .route("/podcasts/search", get(search))
        .route("/podcasts", get(list).post(subscribe_feed))
        .route("/podcasts/subscriptions", get(list_subscriptions))
        // Episodes (static "episodes" segment — distinct from the :id branch)
        .route("/podcasts/episodes/:eid", get(get_episode))
        .route("/podcasts/episodes/:eid/download", post(download_episode))
        .route(
            "/podcasts/episodes/:eid/stream",
            get(stream_episode).head(stream_episode),
        )
        // Per-show
        .route("/podcasts/:id", get(get_podcast).delete(delete_podcast))
        .route("/podcasts/:id/refresh", post(refresh_podcast))
        .route("/podcasts/:id/auto-download", put(set_auto_download))
        .route("/podcasts/:id/episodes", get(list_episodes))
        .route(
            "/podcasts/:id/subscribe",
            post(subscribe).delete(unsubscribe).get(is_subscribed),
        )
        .route("/podcasts/:id/cover", get(serve_cover))
}

// ---------------------------------------------------------------------------
// Helpers / DTOs
// ---------------------------------------------------------------------------

fn caller_id(req: &Request<Body>) -> Result<Identity, ApiError> {
    req.extensions()
        .get::<Identity>()
        .cloned()
        .ok_or_else(|| AppError::Unauthenticated("missing identity".into()).into())
}

fn svc(state: &RestState) -> Result<&PodcastService, ApiError> {
    state
        .podcasts
        .as_ref()
        .ok_or_else(|| AppError::NotFound("podcasts are not enabled (set PODCAST_PATH)".into()).into())
}

#[derive(Serialize)]
pub struct PodcastDto {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
    pub auto_download: i32,
    pub last_refreshed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
fn podcast_dto(p: m::Podcast) -> PodcastDto {
    PodcastDto {
        id: p.id.to_string(),
        feed_url: p.feed_url,
        title: p.title,
        author: p.author,
        description: p.description,
        image_url: p.image_url,
        link: p.link,
        language: p.language,
        categories: serde_json::from_str(&p.categories).unwrap_or_default(),
        itunes_id: p.itunes_id,
        podcastindex_id: p.podcastindex_id,
        auto_download: p.auto_download,
        last_refreshed_at: p.last_refreshed_at.map(rfc3339),
        created_at: rfc3339(p.created_at),
        updated_at: rfc3339(p.updated_at),
    }
}

#[derive(Serialize)]
pub struct EpisodeDto {
    pub id: String,
    pub podcast_id: String,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub episode_no: Option<i32>,
    pub season_no: Option<i32>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i32>,
    pub file_size: Option<i64>,
    pub published_at: Option<String>,
    pub downloaded: bool,
}
fn episode_dto(e: m::PodcastEpisode) -> EpisodeDto {
    EpisodeDto {
        id: e.id.to_string(),
        podcast_id: e.podcast_id.to_string(),
        guid: e.guid,
        title: e.title,
        description: e.description,
        enclosure_url: e.enclosure_url,
        enclosure_type: e.enclosure_type,
        episode_no: e.episode_no,
        season_no: e.season_no,
        duration_ms: e.duration_ms,
        codec: e.codec,
        bitrate_kbps: e.bitrate_kbps,
        file_size: e.file_size,
        published_at: e.published_at.map(rfc3339),
        downloaded: e.file_path.is_some(),
    }
}

#[derive(Serialize)]
pub struct CandidateDto {
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Discovery + catalog
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    pub term: String,
    pub limit: Option<i64>,
}

async fn search(
    State(state): State<RestState>,
    Query(q): Query<SearchQuery>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = caller_id(&req)?;
    let cands = svc(&state)?
        .search(&caller, &q.term, q.limit.unwrap_or(50))
        .await?;
    let candidates: Vec<CandidateDto> = cands
        .into_iter()
        .map(|c| CandidateDto {
            feed_url: c.feed_url,
            title: c.title,
            author: c.author,
            description: c.description,
            image_url: c.image_url,
            categories: c.categories,
            itunes_id: c.itunes_id,
            podcastindex_id: c.podcastindex_id,
        })
        .collect();
    Ok(Json(serde_json::json!({ "candidates": candidates })))
}

#[derive(Deserialize)]
pub struct SubscribeFeedBody {
    #[serde(default)]
    pub feed_url: Option<String>,
    #[serde(default)]
    pub itunes_id: Option<i64>,
}

async fn subscribe_feed(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<PodcastDto>, ApiError> {
    let caller = caller_id(&req)?;
    let body: SubscribeFeedBody = crate::rest::parse_json(req).await?;
    let feed_url = body.feed_url.filter(|s| !s.trim().is_empty());
    let p = svc(&state)?
        .subscribe_feed(&caller, feed_url.as_deref(), body.itunes_id)
        .await?;
    Ok(Json(podcast_dto(p)))
}

#[derive(Deserialize)]
pub struct PageQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

async fn list(
    State(state): State<RestState>,
    Query(q): Query<PageQuery>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = caller_id(&req)?;
    let (items, total) = svc(&state)?.list(&caller, q.limit, q.offset).await?;
    Ok(Json(serde_json::json!({
        "podcasts": items.into_iter().map(podcast_dto).collect::<Vec<_>>(),
        "total": total,
    })))
}

async fn get_podcast(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<PodcastDto>, ApiError> {
    let caller = caller_id(&req)?;
    let p = svc(&state)?.get(&caller, id).await?;
    Ok(Json(podcast_dto(p)))
}

async fn delete_podcast(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = caller_id(&req)?;
    svc(&state)?.delete(&caller, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn refresh_podcast(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = caller_id(&req)?;
    let report = svc(&state)?.refresh(&caller, id).await?;
    Ok(Json(serde_json::json!({
        "podcast_id": report.podcast_id.to_string(),
        "new_episodes": report.new_episodes,
        "not_modified": report.not_modified,
    })))
}

#[derive(Deserialize)]
pub struct AutoDownloadBody {
    pub auto_download: i32,
}

async fn set_auto_download(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<PodcastDto>, ApiError> {
    let caller = caller_id(&req)?;
    let body: AutoDownloadBody = crate::rest::parse_json(req).await?;
    let p = svc(&state)?
        .set_auto_download(&caller, id, body.auto_download)
        .await?;
    Ok(Json(podcast_dto(p)))
}

// ---------------------------------------------------------------------------
// Episodes
// ---------------------------------------------------------------------------

async fn list_episodes(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = caller_id(&req)?;
    let eps = svc(&state)?
        .list_episodes(&caller, id, q.limit, q.offset)
        .await?;
    Ok(Json(serde_json::json!({
        "episodes": eps.into_iter().map(episode_dto).collect::<Vec<_>>(),
    })))
}

async fn get_episode(
    State(state): State<RestState>,
    Path(eid): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<EpisodeDto>, ApiError> {
    let caller = caller_id(&req)?;
    let e = svc(&state)?.get_episode(&caller, eid).await?;
    Ok(Json(episode_dto(e)))
}

async fn download_episode(
    State(state): State<RestState>,
    Path(eid): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<EpisodeDto>, ApiError> {
    let caller = caller_id(&req)?;
    let e = svc(&state)?.download_episode(&caller, eid).await?;
    Ok(Json(episode_dto(e)))
}

/// `GET|HEAD /podcasts/episodes/:eid/stream` — byte-range serve of a downloaded
/// episode, reusing the track streaming helper. A not-downloaded episode 404s
/// (the client streams the enclosure URL from origin instead).
async fn stream_episode(
    State(state): State<RestState>,
    Path(eid): Path<Uuid>,
    headers: HeaderMap,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    let caller = caller_id(&req)?;
    let method = req.method().clone();
    let resolved = state.streaming.resolve_episode(&caller, eid).await?;
    crate::rest::streaming::serve_resolved(resolved, &headers, method).await
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SubscribedDto {
    pub subscribed: bool,
}

async fn subscribe(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<SubscribedDto>, ApiError> {
    let caller = caller_id(&req)?;
    svc(&state)?.subscribe(&caller, id).await?;
    Ok(Json(SubscribedDto { subscribed: true }))
}

async fn unsubscribe(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<StatusCode, ApiError> {
    let caller = caller_id(&req)?;
    svc(&state)?.unsubscribe(&caller, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn is_subscribed(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<Json<SubscribedDto>, ApiError> {
    let caller = caller_id(&req)?;
    let subscribed = svc(&state)?.is_subscribed(&caller, id).await?;
    Ok(Json(SubscribedDto { subscribed }))
}

async fn list_subscriptions(
    State(state): State<RestState>,
    req: Request<Body>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caller = caller_id(&req)?;
    let items = svc(&state)?.list_subscriptions(&caller).await?;
    let total = items.len() as i64;
    Ok(Json(serde_json::json!({
        "podcasts": items.into_iter().map(podcast_dto).collect::<Vec<_>>(),
        "total": total,
    })))
}

// ---------------------------------------------------------------------------
// Cover art
// ---------------------------------------------------------------------------

async fn serve_cover(
    State(state): State<RestState>,
    Path(id): Path<Uuid>,
    req: Request<Body>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>), ApiError> {
    let caller = caller_id(&req)?;
    let podcast = svc(&state)?.get(&caller, id).await?;
    let path = podcast
        .image_path
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AppError::NotFound("no cover for this podcast".into()))?;
    let path = FsPath::new(&path);
    if !path.is_file() {
        return Err(AppError::NotFound("cached cover file not found".into()).into());
    }
    let bytes = tokio::fs::read(path).await.map_err(AppError::Io)?;
    let mut h = HeaderMap::new();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static(image_content_type(path)),
    );
    Ok((StatusCode::OK, h, bytes))
}

fn image_content_type(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}
