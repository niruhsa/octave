//! Tauri commands for the sync engine (Phase 5).
//!
//! `sync_now` runs a full push→pull→prune cycle. The outbox enqueue
//! commands let the frontend record offline playlist edits that the engine
//! replays on reconnect. `sync_pending_count` powers a "N unsynced edits"
//! badge.

use std::sync::Arc;

use tauri::State;

use crate::cache::repo;
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
pub async fn sync_now(state: State<'_, AppStateHandle>) -> AppResult<SyncReport> {
    engine(&state).await?.sync_now().await
}

/// Number of queued offline edits awaiting sync.
#[tauri::command]
pub async fn sync_pending_count(state: State<'_, AppStateHandle>) -> AppResult<i64> {
    repo::count_pending_ops(&state.pool).await
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
