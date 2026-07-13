//! Tauri commands for podcasts.
//!
//! Reads go through `PodcastService` (server when online, cache fallback);
//! search + subscribe/unsubscribe + refresh + auto-download are
//! server-authoritative. Episode downloads live in `download_commands` so they
//! reuse the music `DownloadManager`.

use tauri::State;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::library::LibraryView;
use crate::podcasts::service::PodcastService;
use crate::podcasts::{MergedEpisode, MergedPodcast};
use crate::transport::{PodcastCandidate, RefreshReport};
use crate::AppStateHandle;

const DEFAULT_LIMIT: i64 = 50;

async fn service<'a>(state: &'a State<'a, AppStateHandle>) -> AppResult<PodcastService<'a>> {
    let auth: std::sync::Arc<AuthManager> = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    Ok(PodcastService::new(&state.pool, auth))
}

fn normalise_limit(limit: Option<i64>) -> i64 {
    let l = limit.unwrap_or(DEFAULT_LIMIT);
    if l <= 0 {
        DEFAULT_LIMIT
    } else {
        l.min(200)
    }
}

#[tauri::command]
pub async fn podcast_search(
    state: State<'_, AppStateHandle>,
    query: String,
    limit: Option<i64>,
) -> AppResult<Vec<PodcastCandidate>> {
    let svc = service(&state).await?;
    svc.search(&query, normalise_limit(limit)).await
}

#[tauri::command]
pub async fn podcast_list(
    state: State<'_, AppStateHandle>,
) -> AppResult<LibraryView<MergedPodcast>> {
    let svc = service(&state).await?;
    svc.list_subscriptions().await
}

#[tauri::command]
pub async fn podcast_get(state: State<'_, AppStateHandle>, id: String) -> AppResult<MergedPodcast> {
    let svc = service(&state).await?;
    svc.get_podcast(&id).await
}

#[tauri::command]
pub async fn podcast_list_episodes(
    state: State<'_, AppStateHandle>,
    podcast_id: String,
) -> AppResult<LibraryView<MergedEpisode>> {
    let svc = service(&state).await?;
    svc.list_episodes(&podcast_id).await
}

#[tauri::command]
pub async fn podcast_record_progress(
    state: State<'_, AppStateHandle>,
    episode_id: String,
    position_ms: i64,
    completed: bool,
) -> AppResult<()> {
    let svc = service(&state).await?;
    svc.record_progress(&episode_id, position_ms, completed)
        .await
}

#[tauri::command]
pub async fn podcast_subscribe_feed(
    state: State<'_, AppStateHandle>,
    feed_url: Option<String>,
    itunes_id: Option<i64>,
) -> AppResult<MergedPodcast> {
    let svc = service(&state).await?;
    svc.subscribe_feed(feed_url.as_deref(), itunes_id).await
}

#[tauri::command]
pub async fn podcast_subscribe(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<MergedPodcast> {
    let svc = service(&state).await?;
    svc.subscribe(&id).await
}

#[tauri::command]
pub async fn podcast_unsubscribe(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<MergedPodcast> {
    let svc = service(&state).await?;
    svc.unsubscribe(&id).await
}

#[tauri::command]
pub async fn podcast_refresh(
    state: State<'_, AppStateHandle>,
    id: String,
) -> AppResult<RefreshReport> {
    let svc = service(&state).await?;
    svc.refresh(&id).await
}

#[tauri::command]
pub async fn podcast_set_auto_download(
    state: State<'_, AppStateHandle>,
    id: String,
    auto_download: i32,
) -> AppResult<MergedPodcast> {
    let svc = service(&state).await?;
    svc.set_auto_download(&id, auto_download).await
}
