//! Tauri commands for the sync engine (Phase 5).
//!
//! `sync_now` runs a full push→pull→prune cycle. The outbox enqueue
//! commands let the frontend record offline playlist edits that the engine
//! replays on reconnect. `sync_pending_count` powers a "N unsynced edits"
//! badge.

use std::sync::Arc;

use tauri::State;

use crate::cache::repo;
use crate::equalizer::EqualizerService;
use crate::error::{AppError, AppResult};
use crate::sync::{PendingOpKind, SyncEngine, SyncReport};
use crate::AppStateHandle;

async fn engine(state: &State<'_, AppStateHandle>) -> AppResult<SyncEngine> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AppError::AuthNotConfigured("call auth_configure_server first".into()))?
    };
    Ok(SyncEngine::new(state.pool.clone(), Arc::clone(&auth)))
}

/// Full reconcile: replay outbox, pull cached entities, prune missing files.
#[tauri::command]
pub async fn sync_now(
    state: State<'_, AppStateHandle>,
    equalizer: State<'_, Arc<EqualizerService>>,
) -> AppResult<SyncReport> {
    // The domains are independent. Always give EQ a chance to reconcile even
    // when a library/playlist sync fails; preserve the established command
    // result by returning the legacy error after both attempts complete.
    let legacy = match engine(&state).await {
        Ok(engine) => engine.sync_now().await,
        Err(error) => Err(error),
    };
    // EQ owns a separate account-scoped outbox because the original playlist
    // outbox predates account/server scoping. It still shares this scheduler so
    // reconnect/focus/interval syncs converge both domains.
    let equalizer_result = equalizer.sync_now().await;
    match (legacy, equalizer_result) {
        (Ok(report), Ok(_)) => Ok(report),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

/// Number of queued offline edits awaiting sync.
#[tauri::command]
pub async fn sync_pending_count(
    state: State<'_, AppStateHandle>,
    equalizer: State<'_, Arc<EqualizerService>>,
) -> AppResult<i64> {
    let legacy = repo::count_pending_ops(&state.pool).await?;
    let scoped_equalizer = equalizer.snapshot().await?.pending_count;
    Ok(legacy + scoped_equalizer)
}

/// Append a typed op to the offline-edit outbox. The frontend calls this
/// when a playlist mutation is made while offline (or always, letting the
/// engine be the single writer to the server). Returns the new op id.
#[tauri::command]
pub async fn sync_enqueue_op(
    state: State<'_, AppStateHandle>,
    op: PendingOpKind,
) -> AppResult<i64> {
    let payload = op.to_payload_json()?;
    repo::enqueue_op(&state.pool, op.op_type(), &payload).await
}
