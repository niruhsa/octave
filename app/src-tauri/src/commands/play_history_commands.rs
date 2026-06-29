//! Tauri commands for play history (Phase 11).
//!
//! Recording is **offline-first**: `play_history_record` queues a play into the
//! local `pending_plays` outbox (always succeeds, even offline). The sync
//! scheduler — and an opportunistic call right after recording — drains the
//! outbox to the server via `play_history_flush`. Reads (`*_list` / `*_stats`)
//! are server-authoritative and online-only, like the notifications feed.

use std::sync::Arc;

use tauri::State;
use uuid::Uuid;

use crate::auth::AuthManager;
use crate::cache::repo;
use crate::error::{AppError, AppResult};
use crate::transport::{ListeningStats, PlayHistoryPage, PlayInput};
use crate::AppStateHandle;

/// Max plays pushed in a single flush — bounds an offline backlog's first POST
/// (the server also caps a batch). Remaining rows flush on the next pass.
const FLUSH_BATCH: i64 = 200;

/// Resolve the active `AuthManager`, or error if no server is configured yet.
async fn manager(state: &State<'_, AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .as_ref()
        .cloned()
        .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))
}

/// Queue a play locally (offline-safe). Flushed to the server by
/// [`play_history_flush`]. `played_at` is stamped at insert time (≈ when the
/// play happened, since the client records at the count-as-played threshold).
#[tauri::command]
pub async fn play_history_record(
    state: State<'_, AppStateHandle>,
    track_id: String,
    ms_played: i64,
    completed: bool,
) -> AppResult<()> {
    let id = Uuid::new_v4().to_string();
    repo::enqueue_play(&state.pool, &id, &track_id, ms_played.max(0), completed).await
}

/// Flush queued plays to the server in one batch. Returns the number recorded
/// server-side. Offline / not-yet-configured → `Ok(0)` with the batch kept for
/// a later retry; a permanent rejection (auth/permission) drops the batch so
/// the outbox can't grow unbounded.
#[tauri::command]
pub async fn play_history_flush(state: State<'_, AppStateHandle>) -> AppResult<u64> {
    let pending = repo::list_pending_plays(&state.pool, FLUSH_BATCH).await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let Ok(mgr) = manager(&state).await else {
        return Ok(0); // no server configured yet — keep queued
    };

    let events: Vec<PlayInput> = pending
        .iter()
        .map(|p| PlayInput {
            track_id: p.track_id.clone(),
            ms_played: p.ms_played,
            completed: p.completed != 0,
            played_at: Some(p.played_at.clone()),
        })
        .collect();

    match mgr.record_plays(&events).await {
        Ok(n) => {
            let ids: Vec<String> = pending.into_iter().map(|p| p.id).collect();
            repo::delete_pending_plays(&state.pool, &ids).await?;
            Ok(n)
        }
        // Retryable (offline / server unreachable / not configured) — keep queued.
        Err(AppError::Transport(_)) | Err(AppError::AuthNotConfigured(_)) => Ok(0),
        // Permanent (e.g. a SECRET_KEY session has no user to own plays) — drop
        // the batch so it can't accumulate forever.
        Err(e) => {
            tracing::warn!(error = %e, "play_history flush: dropping unflushable batch");
            let ids: Vec<String> = pending.into_iter().map(|p| p.id).collect();
            repo::delete_pending_plays(&state.pool, &ids).await?;
            Ok(0)
        }
    }
}

/// A page of the caller's plays, newest first (server-authoritative).
#[tauri::command]
pub async fn play_history_list(
    state: State<'_, AppStateHandle>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<PlayHistoryPage> {
    manager(&state).await?.list_play_history(limit, offset).await
}

/// Aggregate listening stats over a window (`window_days` 0/None = all time).
#[tauri::command]
pub async fn play_history_stats(
    state: State<'_, AppStateHandle>,
    window_days: Option<i64>,
    limit: Option<i64>,
) -> AppResult<ListeningStats> {
    manager(&state)
        .await?
        .play_stats(window_days, limit)
        .await
}
