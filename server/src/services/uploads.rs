//! Uploads v2 — DB-backed, session-oriented, per-chunk-verified uploads.
//!
//! An *upload* is a **session** carrying one or more files. `init` declares the
//! whole session (a list of files, each with its chunk map + per-chunk hashes);
//! `put_chunk` verifies and stores one chunk; when every chunk of every file
//! has landed the session is reassembled, each file's whole-file hash is
//! verified, the files are ingested via [`IngestService`], and an aggregated
//! report is written. Sessions are queryable ([`get`]/[`list`]), cancellable
//! ([`cancel`], which cleans staged chunks off disk), and broadcast live
//! progress via [`UploadHub`].
//!
//! Both transports (gRPC primary, REST/WebSocket fallback) call this one
//! service, so the business logic lives in exactly one place.
//!
//! [`get`]: UploadsService::get
//! [`list`]: UploadsService::list
//! [`cancel`]: UploadsService::cancel

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{
    NewUpload, NewUploadChunk, NewUploadFile, PermissionLevel, Upload, UploadFile, UploadFileState,
    UploadFilter, UploadState,
};
use crate::db::repo::UploadRepo;
use crate::error::{AppError, Result};
use crate::services::archive::ArchiveKind;
use crate::services::ingest::IngestService;

/// Broadcast channel depth. Lagging subscribers drop old events (live progress
/// is fine to skip); they still get the next event.
const HUB_CAPACITY: usize = 512;

/// An active upload with no chunk activity for this long is auto-paused by the
/// server sweeper. Matches the client's own stall monitor (1 minute) — this is
/// the authoritative backstop for when the client can't deliver its own `pause`
/// (its network is down, or it was killed).
const STALL_PAUSE_SECS: i64 = 60;

/// How often the server sweeps for stalled uploads.
const STALL_SWEEP_SECS: u64 = 20;

// ===========================================================================
// Wire input DTOs (REST JSON deserializes these; gRPC builds them from proto)
// ===========================================================================

/// One chunk's declared `[start, end)` range + expected content hash.
#[derive(Debug, Clone, Deserialize)]
pub struct ChunkInit {
    pub index: i32,
    pub start: i64,
    pub end: i64,
    /// SHA-256 (lowercase hex) of this chunk's bytes.
    pub hash: String,
}

/// One file in a session: where to write the reassembled bytes, its whole-file
/// hash, and its chunk map.
#[derive(Debug, Clone, Deserialize)]
pub struct FileInit {
    pub filename: String,
    /// SHA-256 (lowercase hex) of the fully reassembled file.
    pub hash: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_chunks: i32,
    pub chunks: Vec<ChunkInit>,
}

// ===========================================================================
// Output views (serialized for REST + Tauri; mapped to proto for gRPC)
// ===========================================================================

/// Per-chunk detail (admins browse exactly which chunks landed).
#[derive(Debug, Clone, Serialize)]
pub struct ChunkView {
    pub index: i32,
    pub start: i64,
    pub end: i64,
    pub hash: String,
    pub received: bool,
}

/// Per-file detail within a session view.
#[derive(Debug, Clone, Serialize)]
pub struct UploadFileView {
    pub file_index: i32,
    pub filename: String,
    pub file_hash: String,
    pub total_size: i64,
    pub chunk_size: i64,
    pub total_chunks: i32,
    pub received_chunks: i32,
    pub state: UploadFileState,
    pub error: Option<String>,
    pub chunks: Vec<ChunkView>,
}

/// Full session report (what `GET /uploads/:id` returns).
#[derive(Debug, Clone, Serialize)]
pub struct UploadView {
    pub id: String,
    pub user_id: Option<String>,
    pub state: UploadState,
    pub total_files: i32,
    pub total_bytes: i64,
    pub bytes_received: i64,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
    /// Aggregated ingest report (parsed from `report_json`), once completed.
    pub report: Option<serde_json::Value>,
    pub files: Vec<UploadFileView>,
}

/// Lightweight row for `GET /uploads` list responses.
#[derive(Debug, Clone, Serialize)]
pub struct UploadSummary {
    pub id: String,
    pub user_id: Option<String>,
    pub state: UploadState,
    pub total_files: i32,
    pub total_bytes: i64,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
}

impl From<Upload> for UploadSummary {
    fn from(u: Upload) -> Self {
        Self {
            id: u.id.to_string(),
            user_id: u.user_id.map(|x| x.to_string()),
            state: u.state,
            total_files: u.total_files,
            total_bytes: u.total_bytes,
            created_at: iso(u.created_at),
            updated_at: iso(u.updated_at),
            error: u.error,
        }
    }
}

/// Outcome of a single `put_chunk`.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkAck {
    pub file_index: i32,
    pub chunk_index: i32,
    pub received_chunks: i32,
    pub total_chunks: i32,
    pub file_complete: bool,
    pub upload_complete: bool,
    pub state: UploadState,
}

// ===========================================================================
// Aggregated completion report (stored as report_json)
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackSummary {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReport {
    pub filename: String,
    pub ok: bool,
    pub error: Option<String>,
    pub is_archive: bool,
    pub archive_kind: Option<String>,
    pub ingested: u64,
    pub already_indexed: u64,
    pub non_audio_skipped: u64,
    pub errors: u64,
    pub track_ids: Vec<String>,
    pub tracks: Vec<TrackSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionReport {
    pub files: Vec<FileReport>,
    pub tracks_ingested: u64,
    pub files_failed: u64,
}

// ===========================================================================
// Live-updates hub
// ===========================================================================

/// A live upload event broadcast to permitted listeners. `owner_id` drives the
/// per-listener permission filter ([`can_see`]).
#[derive(Debug, Clone, Serialize)]
pub struct UploadEvent {
    /// "initialized" | "progress" | "completed" | "cancelled".
    pub kind: String,
    pub upload_id: String,
    pub owner_id: Option<Uuid>,
    pub state: UploadState,
    pub file_index: Option<i32>,
    pub total_files: i32,
    pub bytes_received: i64,
    pub total_bytes: i64,
    pub chunks_received: i32,
    pub total_chunks: i32,
    pub bytes_per_sec: Option<f64>,
    pub report: Option<serde_json::Value>,
}

/// Cloneable broadcast publisher shared across transports + handlers.
#[derive(Clone)]
pub struct UploadHub {
    tx: broadcast::Sender<UploadEvent>,
}

impl Default for UploadHub {
    fn default() -> Self {
        Self::new()
    }
}

impl UploadHub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(HUB_CAPACITY);
        Self { tx }
    }

    /// Subscribe to the live `uploads` stream. The caller is responsible for
    /// applying [`can_see`] before forwarding each event to a listener.
    pub fn subscribe(&self) -> broadcast::Receiver<UploadEvent> {
        self.tx.subscribe()
    }

    fn publish(&self, ev: UploadEvent) {
        // Err only when there are no subscribers — fine to ignore.
        let _ = self.tx.send(ev);
    }
}

/// Whether `listener` is permitted to see an event owned by `owner`.
/// Admin / `SECRET_KEY` see everything; a user sees only their own uploads.
pub fn can_see(listener: &Identity, owner: Option<Uuid>) -> bool {
    if listener.level() == PermissionLevel::Admin {
        return true;
    }
    matches!((listener.user_id(), owner), (Some(c), Some(o)) if c == o)
}

// ===========================================================================
// Service
// ===========================================================================

#[derive(Clone)]
pub struct UploadsService {
    repo: Arc<dyn UploadRepo>,
    ingest: IngestService,
    /// `<INGEST_PATH>/.uploads` — where chunk bytes are staged. `None` when no
    /// ingest root is configured (upload features then error clearly).
    uploads_root: Option<PathBuf>,
    hub: UploadHub,
    /// In-process guard electing a single finalizer per session (prevents a
    /// double ingest if two "last" chunks race).
    finalizing: Arc<Mutex<HashSet<Uuid>>>,
}

impl UploadsService {
    pub fn new(repo: Arc<dyn UploadRepo>, ingest: IngestService, hub: UploadHub) -> Self {
        let uploads_root = ingest.ingest_root.as_ref().map(|r| r.join(".uploads"));
        Self {
            repo,
            ingest,
            uploads_root,
            hub,
            finalizing: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn hub(&self) -> &UploadHub {
        &self.hub
    }

    // ------------------------------------------------------------------
    // init
    // ------------------------------------------------------------------

    /// Declare a new upload session. Manager+. Rejects if the caller already
    /// has an in-flight upload (one at a time).
    pub async fn init(&self, caller: &Identity, files: Vec<FileInit>) -> Result<UploadView> {
        caller.require(PermissionLevel::Manager)?;
        self.uploads_root()?; // fail fast if staging dir isn't configured

        if files.is_empty() {
            return Err(AppError::InvalidArgument("upload requires at least one file".into()));
        }
        for (i, f) in files.iter().enumerate() {
            validate_file(i, f)?;
        }

        // One active upload per user (best-effort; single-client scenario).
        if self.repo.count_active_for_user(caller.user_id()).await? > 0 {
            return Err(AppError::InvalidArgument(
                "an upload is already in progress; cancel it before starting another".into(),
            ));
        }

        let total_bytes: i64 = files.iter().map(|f| f.total_size).sum();
        let upload = self
            .repo
            .create_upload(NewUpload {
                user_id: caller.user_id(),
                total_files: files.len() as i32,
                total_bytes,
            })
            .await?;

        for (idx, f) in files.iter().enumerate() {
            let file = self
                .repo
                .create_file(NewUploadFile {
                    upload_id: upload.id,
                    file_index: idx as i32,
                    filename: f.filename.clone(),
                    file_hash: f.hash.to_ascii_lowercase(),
                    total_size: f.total_size,
                    chunk_size: f.chunk_size,
                    total_chunks: f.total_chunks,
                })
                .await?;
            for c in &f.chunks {
                self.repo
                    .create_chunk(NewUploadChunk {
                        upload_file_id: file.id,
                        chunk_index: c.index,
                        start_byte: c.start,
                        end_byte: c.end,
                        hash: c.hash.to_ascii_lowercase(),
                    })
                    .await?;
            }
        }

        debug!(upload_id = %upload.id, files = files.len(), "upload session initialized");
        let view = self.build_view(&upload).await?;
        self.publish_state(&upload, "initialized", None, &view.files_models).await;
        Ok(view.view)
    }

    // ------------------------------------------------------------------
    // put_chunk
    // ------------------------------------------------------------------

    /// Receive + verify one chunk. On a hash mismatch the chunk is **rejected**
    /// (`InvalidArgument`) and nothing is persisted. When the chunk completes
    /// the session, reassembly + ingest run and the report is written.
    pub async fn put_chunk(
        &self,
        caller: &Identity,
        upload_id: Uuid,
        file_index: i32,
        chunk_index: i32,
        bytes: &[u8],
    ) -> Result<ChunkAck> {
        caller.require(PermissionLevel::Manager)?;
        let upload = self.load(upload_id).await?;
        authorize_owner_or_admin(caller, &upload)?;
        if !upload.state.is_active() {
            return Err(AppError::InvalidArgument(format!(
                "upload is {:?}; not accepting chunks",
                upload.state
            )));
        }

        let file = self
            .repo
            .get_file(upload_id, file_index)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("file index {file_index}")))?;
        let chunk = self
            .repo
            .get_chunk(file.id, chunk_index)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("chunk index {chunk_index}")))?;

        // Size must match the declared range.
        let expected = (chunk.end_byte - chunk.start_byte).max(0) as usize;
        if bytes.len() != expected {
            return Err(AppError::InvalidArgument(format!(
                "chunk {chunk_index} size {} != expected {expected}",
                bytes.len()
            )));
        }

        // Verify content hash BEFORE writing anything to disk. A mismatch hard-
        // fails this chunk and leaves all state untouched (client must re-send).
        let got = hex_lower(&Sha256::digest(bytes));
        if got != chunk.hash.to_ascii_lowercase() {
            warn!(upload_id = %upload_id, file_index, chunk_index, "chunk hash mismatch — rejected");
            return Err(AppError::InvalidArgument(format!(
                "chunk {chunk_index} hash mismatch (corruption); re-upload this chunk"
            )));
        }

        // Persist the bytes (temp → rename so a partial write never looks done).
        let file_dir = self.file_dir(upload_id, file_index)?;
        tokio::fs::create_dir_all(&file_dir)
            .await
            .map_err(|e| AppError::Internal(format!("create chunk dir: {e}")))?;
        let tmp = file_dir.join(format!("{chunk_index}.part.tmp"));
        let final_path = file_dir.join(format!("{chunk_index}.part"));
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| AppError::Internal(format!("write chunk: {e}")))?;
        tokio::fs::rename(&tmp, &final_path)
            .await
            .map_err(|e| AppError::Internal(format!("commit chunk: {e}")))?;

        let (received_chunks, total_chunks) =
            self.repo.mark_chunk_received(file.id, chunk_index).await?;
        let file_complete = received_chunks >= total_chunks;
        if file_complete {
            self.repo
                .set_file_state(file.id, UploadFileState::Complete, None)
                .await?;
        }
        // initialized → uploading on the first chunk. A chunk landing while
        // `paused` is still accepted + stored (paused is_active), but does NOT
        // flip the state here: the client drives resume explicitly (manual, or
        // its first successful chunk after a stall calls `resume`). This keeps a
        // *manual* pause consistent — the few in-flight chunks at pause time land
        // without spuriously un-pausing the session.
        if upload.state == UploadState::Initialized {
            self.repo
                .set_upload_state(upload_id, UploadState::Uploading)
                .await?;
        }

        // Recompute session-wide progress for the broadcast + completion check.
        let files = self.repo.list_files(upload_id).await?;
        let upload_complete = files.iter().all(|f| f.received_chunks >= f.total_chunks);
        self.publish_progress(&upload, Some(file_index), &files).await;

        if upload_complete {
            self.maybe_finalize(caller, upload_id).await?;
        }

        Ok(ChunkAck {
            file_index,
            chunk_index,
            received_chunks,
            total_chunks,
            file_complete,
            upload_complete,
            state: if upload_complete {
                UploadState::Completed
            } else {
                UploadState::Uploading
            },
        })
    }

    // ------------------------------------------------------------------
    // get / list / cancel
    // ------------------------------------------------------------------

    pub async fn get(&self, caller: &Identity, id: Uuid) -> Result<UploadView> {
        let upload = self.load(id).await?;
        authorize_owner_or_admin(caller, &upload)?;
        Ok(self.build_view(&upload).await?.view)
    }

    /// List sessions. Non-admins are restricted to their own; admins may filter
    /// by `requested_user` (or see everyone when `None`).
    pub async fn list(
        &self,
        caller: &Identity,
        requested_user: Option<Uuid>,
        state: Option<UploadState>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<UploadSummary>> {
        let is_admin = caller.level() == PermissionLevel::Admin;
        let user_filter = if is_admin {
            requested_user
        } else {
            // A non-admin may only see their own; reject an explicit other id.
            if requested_user.is_some() && requested_user != caller.user_id() {
                return Err(AppError::PermissionDenied(
                    "cannot list another user's uploads".into(),
                ));
            }
            caller.user_id()
        };
        let rows = self
            .repo
            .list_uploads(
                UploadFilter { user_id: user_filter, state },
                limit.clamp(1, 200),
                offset.max(0),
            )
            .await?;
        Ok(rows.into_iter().map(UploadSummary::from).collect())
    }

    /// Cancel an in-flight session: mark `cancelled` and delete its staged
    /// chunks from disk.
    pub async fn cancel(&self, caller: &Identity, id: Uuid) -> Result<UploadView> {
        let upload = self.load(id).await?;
        authorize_owner_or_admin(caller, &upload)?;
        if !upload.state.is_active() {
            return Err(AppError::InvalidArgument(format!(
                "upload is {:?}; nothing to cancel",
                upload.state
            )));
        }
        self.repo
            .set_upload_state(id, UploadState::Cancelled)
            .await?;
        if let Ok(dir) = self.upload_dir(id) {
            // Best-effort: the dir may not exist yet if no chunk landed.
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        let upload = self.load(id).await?;
        let built = self.build_view(&upload).await?;
        self.publish_state(&upload, "cancelled", None, &built.files_models).await;
        Ok(built.view)
    }

    /// Pause an in-flight session. The client stops sending chunks; the session
    /// stays staged + resumable. Idempotent if already paused. A completed /
    /// cancelled session can't be paused.
    pub async fn pause(&self, caller: &Identity, id: Uuid) -> Result<UploadView> {
        let upload = self.load(id).await?;
        authorize_owner_or_admin(caller, &upload)?;
        if !upload.state.is_active() {
            return Err(AppError::InvalidArgument(format!(
                "upload is {:?}; cannot pause",
                upload.state
            )));
        }
        if upload.state != UploadState::Paused {
            self.repo.set_upload_state(id, UploadState::Paused).await?;
        }
        let upload = self.load(id).await?;
        let built = self.build_view(&upload).await?;
        self.publish_state(&upload, "paused", None, &built.files_models).await;
        Ok(built.view)
    }

    /// Resume a paused session back to `uploading`. A chunk landing also
    /// auto-resumes (see [`put_chunk`]); this is the explicit/manual path. Only
    /// a paused session can be resumed (others are a no-op error).
    ///
    /// [`put_chunk`]: UploadsService::put_chunk
    pub async fn resume(&self, caller: &Identity, id: Uuid) -> Result<UploadView> {
        let upload = self.load(id).await?;
        authorize_owner_or_admin(caller, &upload)?;
        if upload.state != UploadState::Paused {
            return Err(AppError::InvalidArgument(format!(
                "upload is {:?}; not paused",
                upload.state
            )));
        }
        self.repo.set_upload_state(id, UploadState::Uploading).await?;
        let upload = self.load(id).await?;
        let built = self.build_view(&upload).await?;
        self.publish_state(&upload, "resumed", None, &built.files_models).await;
        Ok(built.view)
    }

    // ------------------------------------------------------------------
    // stall sweeper (server-side auto-pause backstop)
    // ------------------------------------------------------------------

    /// Spawn the background stall-sweeper: every [`STALL_SWEEP_SECS`] it marks
    /// active uploads idle for ≥ [`STALL_PAUSE_SECS`] as `paused`. This makes the
    /// server set `paused` on its own when chunks simply stop arriving — without
    /// relying on the client's best-effort `pause` call, which fails in the very
    /// case that matters (the client's network went down, or it was killed).
    /// Runs for the life of the process.
    pub fn spawn_stall_sweeper(&self) {
        let svc = self.clone();
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(STALL_SWEEP_SECS));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                if let Err(e) = svc.sweep_stalled().await {
                    warn!(error = %e, "upload stall sweep failed");
                }
            }
        });
    }

    /// One sweep pass: pause every active upload idle for ≥ `STALL_PAUSE_SECS`
    /// and broadcast a `paused` event for each so live listeners update.
    async fn sweep_stalled(&self) -> Result<()> {
        let cutoff = OffsetDateTime::now_utc() - time::Duration::seconds(STALL_PAUSE_SECS);
        let paused = self.repo.pause_stale_active(cutoff).await?;
        for upload in &paused {
            debug!(upload_id = %upload.id, "auto-paused — no chunk activity for ≥{STALL_PAUSE_SECS}s");
            let files = self.repo.list_files(upload.id).await.unwrap_or_default();
            self.publish_state(upload, "paused", None, &files).await;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // finalize (reassemble + verify + ingest + report)
    // ------------------------------------------------------------------

    async fn maybe_finalize(&self, caller: &Identity, upload_id: Uuid) -> Result<()> {
        // Elect a single finalizer for this session.
        {
            let mut guard = self.finalizing.lock().await;
            if guard.contains(&upload_id) {
                return Ok(());
            }
            guard.insert(upload_id);
        }
        let result = self.finalize(caller, upload_id).await;
        self.finalizing.lock().await.remove(&upload_id);
        result
    }

    async fn finalize(&self, caller: &Identity, upload_id: Uuid) -> Result<()> {
        let upload = self.load(upload_id).await?;
        // Re-check: a concurrent cancel may have raced in.
        if !upload.state.is_active() {
            return Ok(());
        }
        let files = self.repo.list_files(upload_id).await?;
        let mut report = SessionReport::default();
        let mut first_error: Option<String> = None;

        for file in &files {
            match self.reassemble_and_ingest_file(caller, upload_id, file).await {
                Ok(fr) => {
                    report.tracks_ingested += fr.ingested + fr.already_indexed;
                    if !fr.ok {
                        report.files_failed += 1;
                        first_error.get_or_insert_with(|| {
                            fr.error.clone().unwrap_or_else(|| "file failed".into())
                        });
                    }
                    let _ = self
                        .repo
                        .set_file_state(
                            file.id,
                            if fr.ok {
                                UploadFileState::Complete
                            } else {
                                UploadFileState::Failed
                            },
                            fr.error.as_deref(),
                        )
                        .await;
                    // Reflect the organised on-disk name in the file row too, so
                    // the per-file view matches the report (no opaque names).
                    if fr.filename != file.filename {
                        let _ = self.repo.set_file_filename(file.id, &fr.filename).await;
                    }
                    report.files.push(fr);
                }
                Err(e) => {
                    let msg = e.to_string();
                    report.files_failed += 1;
                    first_error.get_or_insert_with(|| msg.clone());
                    let _ = self
                        .repo
                        .set_file_state(file.id, UploadFileState::Failed, Some(&msg))
                        .await;
                    report.files.push(FileReport {
                        filename: file.filename.clone(),
                        ok: false,
                        error: Some(msg),
                        is_archive: false,
                        archive_kind: None,
                        ingested: 0,
                        already_indexed: 0,
                        non_audio_skipped: 0,
                        errors: 1,
                        track_ids: vec![],
                        tracks: vec![],
                    });
                }
            }
        }

        let report_json = serde_json::to_string(&report).ok();
        self.repo
            .set_upload_report(
                upload_id,
                UploadState::Completed,
                report_json.as_deref(),
                first_error.as_deref(),
            )
            .await?;

        // Free the staged chunk bytes — the report is the durable artifact now.
        if let Ok(dir) = self.upload_dir(upload_id) {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }

        debug!(upload_id = %upload_id, tracks = report.tracks_ingested, "upload finalized");
        let completed = self.load(upload_id).await?;
        self.publish_completed(&completed, &report).await;
        Ok(())
    }

    /// Concatenate a file's chunks in order, verify the whole-file hash, then
    /// ingest (archive → multi-track, else single). Returns a per-file report.
    async fn reassemble_and_ingest_file(
        &self,
        caller: &Identity,
        upload_id: Uuid,
        file: &UploadFile,
    ) -> Result<FileReport> {
        let ingest_root = self.ingest.ingest_root.as_deref().unwrap_or(Path::new("."));
        let stage = ingest_root.join(".tmp").join(Uuid::new_v4().to_string());
        tokio::fs::create_dir_all(&stage)
            .await
            .map_err(|e| AppError::Internal(format!("create stage dir: {e}")))?;
        let ext = Path::new(&file.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let source = stage.join(format!("source.{ext}"));

        // Concatenate chunks 0..total_chunks, hashing as we go.
        let file_dir = self.file_dir(upload_id, file.file_index)?;
        let mut hasher = Sha256::new();
        {
            let mut out = tokio::fs::File::create(&source)
                .await
                .map_err(|e| AppError::Internal(format!("create source: {e}")))?;
            for i in 0..file.total_chunks {
                let bytes = tokio::fs::read(file_dir.join(format!("{i}.part")))
                    .await
                    .map_err(|e| AppError::Internal(format!("read chunk {i}: {e}")))?;
                hasher.update(&bytes);
                out.write_all(&bytes)
                    .await
                    .map_err(|e| AppError::Internal(format!("write source: {e}")))?;
            }
            out.flush().await.ok();
        }

        let got = hex_lower(&hasher.finalize());
        if got != file.file_hash.to_ascii_lowercase() {
            let _ = tokio::fs::remove_dir_all(&stage).await;
            return Err(AppError::InvalidArgument(format!(
                "{}: reassembled file hash mismatch",
                file.filename
            )));
        }

        // Ingest.
        let fr = if let Some(kind) = ArchiveKind::detect(Path::new(&file.filename)) {
            let res = self.ingest.organize_archive(caller, &source, kind).await;
            let _ = tokio::fs::remove_dir_all(&stage).await;
            let res = res?;
            let tracks = self.track_summaries(&res.track_ids).await;
            FileReport {
                filename: file.filename.clone(),
                ok: res.errors == 0,
                error: (res.errors > 0).then(|| format!("{} member(s) failed", res.errors)),
                is_archive: true,
                archive_kind: Some(format!("{kind:?}")),
                ingested: res.ingested,
                already_indexed: res.already_indexed,
                non_audio_skipped: res.non_audio_skipped,
                errors: res.errors,
                track_ids: res.track_ids.iter().map(|id| id.to_string()).collect(),
                tracks,
            }
        } else {
            let res = self.ingest.organize_and_index(caller, &source).await;
            let _ = tokio::fs::remove_dir_all(&stage).await;
            let res = res?;
            let tracks = self.track_summaries(&[res.track_id]).await;
            // Report the organised on-disk filename (derived from the file's
            // tags), not the name declared at init — which on Android is an
            // opaque content-URI id like `msf_12345.flac`.
            let saved_name = res
                .dest
                .file_name()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| file.filename.clone());
            FileReport {
                filename: saved_name,
                ok: true,
                error: None,
                is_archive: false,
                archive_kind: None,
                ingested: if res.already_indexed { 0 } else { 1 },
                already_indexed: if res.already_indexed { 1 } else { 0 },
                non_audio_skipped: 0,
                errors: 0,
                track_ids: vec![res.track_id.to_string()],
                tracks,
            }
        };
        Ok(fr)
    }

    /// Resolve `{id, title, artist, album}` summaries for a set of track ids
    /// (best-effort; missing rows are skipped).
    async fn track_summaries(&self, ids: &[Uuid]) -> Vec<TrackSummary> {
        let lib = &self.ingest.scan.library;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Ok(Some(track)) = lib.tracks.get(*id).await else {
                continue;
            };
            let album = lib
                .albums
                .get(track.album_id)
                .await
                .ok()
                .flatten()
                .map(|a| a.title)
                .unwrap_or_default();
            let artist = lib
                .artists
                .get(track.artist_id)
                .await
                .ok()
                .flatten()
                .map(|a| a.name)
                .unwrap_or_default();
            out.push(TrackSummary {
                id: track.id.to_string(),
                title: track.title,
                artist,
                album,
            });
        }
        out
    }

    // ------------------------------------------------------------------
    // View building + event publishing
    // ------------------------------------------------------------------

    async fn load(&self, id: Uuid) -> Result<Upload> {
        self.repo
            .get_upload(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("upload {id}")))
    }

    async fn build_view(&self, upload: &Upload) -> Result<BuiltView> {
        let files = self.repo.list_files(upload.id).await?;
        let mut file_views = Vec::with_capacity(files.len());
        for f in &files {
            let chunks = self.repo.list_chunks(f.id).await?;
            file_views.push(UploadFileView {
                file_index: f.file_index,
                filename: f.filename.clone(),
                file_hash: f.file_hash.clone(),
                total_size: f.total_size,
                chunk_size: f.chunk_size,
                total_chunks: f.total_chunks,
                received_chunks: f.received_chunks,
                state: f.state,
                error: f.error.clone(),
                chunks: chunks
                    .into_iter()
                    .map(|c| ChunkView {
                        index: c.chunk_index,
                        start: c.start_byte,
                        end: c.end_byte,
                        hash: c.hash,
                        received: c.received,
                    })
                    .collect(),
            });
        }
        let report = upload
            .report_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        let view = UploadView {
            id: upload.id.to_string(),
            user_id: upload.user_id.map(|x| x.to_string()),
            state: upload.state,
            total_files: upload.total_files,
            total_bytes: upload.total_bytes,
            bytes_received: session_bytes_received(&files),
            created_at: iso(upload.created_at),
            updated_at: iso(upload.updated_at),
            error: upload.error.clone(),
            report,
            files: file_views,
        };
        Ok(BuiltView { view, files_models: files })
    }

    async fn publish_progress(&self, upload: &Upload, file_index: Option<i32>, files: &[UploadFile]) {
        let bytes_received = session_bytes_received(files);
        let (chunks_received, total_chunks) = session_chunks(files);
        self.hub.publish(UploadEvent {
            kind: "progress".into(),
            upload_id: upload.id.to_string(),
            owner_id: upload.user_id,
            state: UploadState::Uploading,
            file_index,
            total_files: upload.total_files,
            bytes_received,
            total_bytes: upload.total_bytes,
            chunks_received,
            total_chunks,
            bytes_per_sec: avg_speed(upload.created_at, bytes_received),
            report: None,
        });
    }

    async fn publish_state(
        &self,
        upload: &Upload,
        kind: &str,
        file_index: Option<i32>,
        files: &[UploadFile],
    ) {
        let (chunks_received, total_chunks) = session_chunks(files);
        self.hub.publish(UploadEvent {
            kind: kind.into(),
            upload_id: upload.id.to_string(),
            owner_id: upload.user_id,
            state: upload.state,
            file_index,
            total_files: upload.total_files,
            bytes_received: session_bytes_received(files),
            total_bytes: upload.total_bytes,
            chunks_received,
            total_chunks,
            bytes_per_sec: None,
            report: None,
        });
    }

    async fn publish_completed(&self, upload: &Upload, report: &SessionReport) {
        self.hub.publish(UploadEvent {
            kind: "completed".into(),
            upload_id: upload.id.to_string(),
            owner_id: upload.user_id,
            state: UploadState::Completed,
            file_index: None,
            total_files: upload.total_files,
            bytes_received: upload.total_bytes,
            total_bytes: upload.total_bytes,
            chunks_received: 0,
            total_chunks: 0,
            bytes_per_sec: None,
            report: serde_json::to_value(report).ok(),
        });
    }

    // ------------------------------------------------------------------
    // Disk path helpers
    // ------------------------------------------------------------------

    fn uploads_root(&self) -> Result<&Path> {
        self.uploads_root
            .as_deref()
            .ok_or_else(|| AppError::Config("INGEST_PATH unset; uploads need a staging dir".into()))
    }

    fn upload_dir(&self, id: Uuid) -> Result<PathBuf> {
        Ok(self.uploads_root()?.join(id.to_string()))
    }

    fn file_dir(&self, id: Uuid, file_index: i32) -> Result<PathBuf> {
        Ok(self.upload_dir(id)?.join(file_index.to_string()))
    }
}

/// Internal carrier so callers that publish can reuse the loaded file rows.
struct BuiltView {
    view: UploadView,
    files_models: Vec<UploadFile>,
}

// ===========================================================================
// Free helpers
// ===========================================================================

fn authorize_owner_or_admin(caller: &Identity, upload: &Upload) -> Result<()> {
    if caller.level() == PermissionLevel::Admin {
        return Ok(());
    }
    match (caller.user_id(), upload.user_id) {
        (Some(c), Some(o)) if c == o => Ok(()),
        _ => Err(AppError::PermissionDenied("not your upload".into())),
    }
}

/// Validate one file's declared chunk map fully: contiguous `[0,total_size)`
/// coverage, correct indices, and well-formed hashes.
fn validate_file(idx: usize, f: &FileInit) -> Result<()> {
    let where_ = || format!("file {idx} ({})", f.filename);
    if f.filename.trim().is_empty() {
        return Err(AppError::InvalidArgument(format!("{}: filename required", where_())));
    }
    if f.total_size <= 0 || f.chunk_size <= 0 {
        return Err(AppError::InvalidArgument(format!(
            "{}: total_size and chunk_size must be > 0",
            where_()
        )));
    }
    if f.total_chunks <= 0 || f.chunks.len() != f.total_chunks as usize {
        return Err(AppError::InvalidArgument(format!(
            "{}: total_chunks must match chunks length",
            where_()
        )));
    }
    if !is_sha256_hex(&f.hash) {
        return Err(AppError::InvalidArgument(format!("{}: invalid file hash", where_())));
    }
    // Verify the chunk map is sorted, contiguous, and covers the whole file.
    let mut chunks = f.chunks.clone();
    chunks.sort_by_key(|c| c.index);
    let mut cursor: i64 = 0;
    for (i, c) in chunks.iter().enumerate() {
        if c.index != i as i32 {
            return Err(AppError::InvalidArgument(format!(
                "{}: chunk indices must be 0..total_chunks",
                where_()
            )));
        }
        if c.start != cursor || c.end <= c.start || c.end > f.total_size {
            return Err(AppError::InvalidArgument(format!(
                "{}: chunk {} range not contiguous",
                where_(),
                c.index
            )));
        }
        if !is_sha256_hex(&c.hash) {
            return Err(AppError::InvalidArgument(format!(
                "{}: chunk {} invalid hash",
                where_(),
                c.index
            )));
        }
        cursor = c.end;
    }
    if cursor != f.total_size {
        return Err(AppError::InvalidArgument(format!(
            "{}: chunks do not cover the whole file",
            where_()
        )));
    }
    Ok(())
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Bytes the server holds for a session (approx: completed chunks × chunk_size,
/// clamped to each file's size so the last short chunk doesn't overshoot).
fn session_bytes_received(files: &[UploadFile]) -> i64 {
    files
        .iter()
        .map(|f| (f.received_chunks as i64 * f.chunk_size).min(f.total_size))
        .sum()
}

fn session_chunks(files: &[UploadFile]) -> (i32, i32) {
    (
        files.iter().map(|f| f.received_chunks).sum(),
        files.iter().map(|f| f.total_chunks).sum(),
    )
}

/// Session-average speed in bytes/sec since `created_at`.
fn avg_speed(created_at: OffsetDateTime, bytes: i64) -> Option<f64> {
    let secs = (OffsetDateTime::now_utc() - created_at).as_seconds_f64();
    if secs > 0.05 && bytes > 0 {
        Some(bytes as f64 / secs)
    } else {
        None
    }
}

fn iso(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(index: i32, start: i64, end: i64) -> ChunkInit {
        ChunkInit { index, start, end, hash: "a".repeat(64) }
    }

    fn file(total: i64, chunk_size: i64, chunks: Vec<ChunkInit>) -> FileInit {
        FileInit {
            filename: "song.flac".into(),
            hash: "b".repeat(64),
            total_size: total,
            chunk_size,
            total_chunks: chunks.len() as i32,
            chunks,
        }
    }

    #[test]
    fn validate_accepts_contiguous_map() {
        let f = file(10, 4, vec![chunk(0, 0, 4), chunk(1, 4, 8), chunk(2, 8, 10)]);
        assert!(validate_file(0, &f).is_ok());
    }

    #[test]
    fn validate_rejects_gap_and_short_coverage() {
        // gap between 4 and 5
        let gap = file(10, 4, vec![chunk(0, 0, 4), chunk(1, 5, 10)]);
        assert!(validate_file(0, &gap).is_err());
        // doesn't cover the whole file
        let short = file(10, 4, vec![chunk(0, 0, 4), chunk(1, 4, 8)]);
        assert!(validate_file(0, &short).is_err());
    }

    #[test]
    fn validate_rejects_bad_hash() {
        let mut f = file(4, 4, vec![chunk(0, 0, 4)]);
        f.hash = "nothex".into();
        assert!(validate_file(0, &f).is_err());
    }

    #[test]
    fn is_sha256_hex_checks_len_and_charset() {
        assert!(is_sha256_hex(&"0".repeat(64)));
        assert!(is_sha256_hex(&"abcdef0123456789".repeat(4)));
        assert!(!is_sha256_hex(&"0".repeat(63)));
        assert!(!is_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn hex_lower_pads() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn paused_is_active_and_parses() {
        // Paused is still in-flight: counts toward the one-at-a-time limit,
        // is cancellable, and accepts chunks (auto-resume).
        assert!(UploadState::Paused.is_active());
        assert!(UploadState::Initialized.is_active());
        assert!(UploadState::Uploading.is_active());
        assert!(!UploadState::Completed.is_active());
        assert!(!UploadState::Cancelled.is_active());
        assert_eq!(UploadState::parse("paused"), Some(UploadState::Paused));
        assert_eq!(UploadState::parse("PAUSED"), Some(UploadState::Paused));
        assert_eq!(UploadState::parse("bogus"), None);
    }

    #[test]
    fn can_see_admin_sees_all_user_sees_own() {
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let admin = Identity::SecretKey;
        let user = Identity::User { id: owner, username: "u".into(), level: PermissionLevel::Manager };
        assert!(can_see(&admin, Some(owner)));
        assert!(can_see(&admin, None));
        assert!(can_see(&user, Some(owner)));
        assert!(!can_see(&user, Some(other)));
        assert!(!can_see(&user, None));
    }

    #[test]
    fn session_progress_helpers() {
        let files = vec![
            UploadFile {
                id: Uuid::new_v4(),
                upload_id: Uuid::new_v4(),
                file_index: 0,
                filename: "a".into(),
                file_hash: "x".into(),
                total_size: 10,
                chunk_size: 4,
                total_chunks: 3,
                received_chunks: 2,
                state: UploadFileState::Uploading,
                error: None,
                created_at: OffsetDateTime::now_utc(),
            },
        ];
        // 2 chunks × 4 = 8, under the 10-byte cap.
        assert_eq!(session_bytes_received(&files), 8);
        assert_eq!(session_chunks(&files), (2, 3));
    }
}
