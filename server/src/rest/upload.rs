//! Chunked, resumable uploads.
//!
//! Flow:
//!   * `POST /upload/init` — declare a file (hash, total size, chunk size,
//!     total chunks, and each chunk's `[start,end)` byte range). The upload is
//!     keyed by the file **hash**, so any device that submits the same hash
//!     resumes the same session. Returns the upload id + which chunks are
//!     already present.
//!   * `POST /upload/chunk/:id/:index` — upload one chunk's raw bytes. When the
//!     final missing chunk arrives the server **reassembles** the chunks in
//!     order into the original filename, verifies the hash, and runs ingest
//!     (archive → multi-file, otherwise single file).
//!   * `GET /upload/status/:id` — which chunks are received / still missing, and
//!     the ingest result once complete.
//!
//! Chunks live under `<ingest_root>/.uploads/<hash>/chunks/<index>.part`; the
//! manifest + (once done) the ingest result sit alongside. Presence on disk is
//! the source of truth for "received", so resume survives a server restart.

use std::path::{Path, PathBuf};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path as AxPath, State},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::error::AppError;
use crate::rest::ApiError;
use crate::rest::ingest::{ArchiveUploadResponse, UploadResponse, UploadResult};
use crate::services::archive::ArchiveKind;

/// Per-chunk body ceiling (64 MiB). Clients pick a much smaller chunk size.
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

pub fn router() -> Router<crate::rest::RestState> {
    Router::new()
        .route("/upload/init", post(init))
        .route("/upload/chunk/:id/:index", post(chunk))
        .route("/upload/status/:id", get(status))
        .route_layer(DefaultBodyLimit::max(MAX_CHUNK_BYTES))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct ChunkMeta {
    index: u32,
    start: u64,
    end: u64,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    upload_id: String,
    filename: String,
    hash: String,
    total_size: u64,
    chunk_size: u64,
    total_chunks: u32,
    chunks: Vec<ChunkMeta>,
}

#[derive(Deserialize)]
struct InitRequest {
    filename: String,
    hash: String,
    total_size: u64,
    chunk_size: u64,
    total_chunks: u32,
    chunks: Vec<ChunkMeta>,
}

#[derive(Serialize)]
struct InitResponse {
    upload_id: String,
    total_chunks: u32,
    received_chunks: Vec<u32>,
}

#[derive(Serialize)]
struct ChunkResponse {
    received: u32,
    total: u32,
    complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<UploadResult>,
}

#[derive(Serialize)]
struct StatusResponse {
    upload_id: String,
    filename: String,
    total_chunks: u32,
    chunk_size: u64,
    total_size: u64,
    received_chunks: Vec<u32>,
    missing_chunks: Vec<u32>,
    complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<UploadResult>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Validate an upload id (a content hash). Hex only + bounded length so it's a
/// safe single path component (no traversal).
fn sanitize_id(id: &str) -> Result<String, AppError> {
    let ok = (16..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(id.to_ascii_lowercase())
    } else {
        Err(AppError::InvalidArgument("invalid upload id".into()))
    }
}

fn uploads_root(state: &crate::rest::RestState) -> Result<PathBuf, AppError> {
    let ingest = state
        .ingest
        .as_ref()
        .ok_or_else(|| AppError::Config("ingest service not configured".into()))?;
    let root = ingest
        .ingest_root
        .clone()
        .ok_or_else(|| AppError::Config("INGEST_PATH unset; chunked upload needs a staging dir".into()))?;
    Ok(root.join(".uploads"))
}

fn upload_dir(state: &crate::rest::RestState, id: &str) -> Result<PathBuf, AppError> {
    Ok(uploads_root(state)?.join(id))
}

/// Indices whose `<index>.part` file exists, sorted.
async fn received_chunks(chunks_dir: &Path, total: u32) -> Vec<u32> {
    let mut got = Vec::new();
    for i in 0..total {
        if tokio::fs::metadata(chunks_dir.join(format!("{i}.part"))).await.is_ok() {
            got.push(i);
        }
    }
    got
}

async fn read_manifest(dir: &Path) -> Result<Manifest, AppError> {
    let bytes = tokio::fs::read(dir.join("manifest.json"))
        .await
        .map_err(|_| AppError::NotFound("upload session not found".into()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Internal(format!("corrupt manifest: {e}")))
}

async fn read_result(dir: &Path) -> Option<UploadResult> {
    let bytes = tokio::fs::read(dir.join("result.json")).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Start or resume an upload. Idempotent for a given hash.
async fn init(
    State(state): State<crate::rest::RestState>,
    Extension(caller): Extension<Identity>,
    Json(body): Json<InitRequest>,
) -> Result<Json<InitResponse>, ApiError> {
    caller.require(PermissionLevel::Manager).map_err(ApiError::from)?;

    let id = sanitize_id(&body.hash)?;
    if body.total_chunks == 0 || body.chunks.len() != body.total_chunks as usize {
        return Err(AppError::InvalidArgument("total_chunks must match chunks length".into()).into());
    }
    if body.total_size == 0 || body.chunk_size == 0 {
        return Err(AppError::InvalidArgument("total_size and chunk_size must be > 0".into()).into());
    }

    let dir = upload_dir(&state, &id)?;
    let chunks_dir = dir.join("chunks");
    tokio::fs::create_dir_all(&chunks_dir)
        .await
        .map_err(|e| AppError::Internal(format!("create upload dir: {e}")))?;

    // (Re)write the manifest — for a resume the chunk files already on disk are
    // preserved, so the client only re-sends what's missing.
    let manifest = Manifest {
        upload_id: id.clone(),
        filename: body.filename,
        hash: id.clone(),
        total_size: body.total_size,
        chunk_size: body.chunk_size,
        total_chunks: body.total_chunks,
        chunks: body.chunks,
    };
    let json = serde_json::to_vec(&manifest)
        .map_err(|e| AppError::Internal(format!("serialize manifest: {e}")))?;
    tokio::fs::write(dir.join("manifest.json"), json)
        .await
        .map_err(|e| AppError::Internal(format!("write manifest: {e}")))?;

    let received = received_chunks(&chunks_dir, manifest.total_chunks).await;
    debug!(upload_id = %id, total = manifest.total_chunks, have = received.len(), "upload init");
    Ok(Json(InitResponse {
        upload_id: id,
        total_chunks: manifest.total_chunks,
        received_chunks: received,
    }))
}

/// Receive one chunk. Reassembles + ingests when the last chunk lands.
async fn chunk(
    State(state): State<crate::rest::RestState>,
    Extension(caller): Extension<Identity>,
    AxPath((id, index)): AxPath<(String, u32)>,
    body: Bytes,
) -> Result<Json<ChunkResponse>, ApiError> {
    caller.require(PermissionLevel::Manager).map_err(ApiError::from)?;

    let id = sanitize_id(&id)?;
    let dir = upload_dir(&state, &id)?;

    // Already finished (e.g. a retried chunk after completion) → return result.
    if let Some(result) = read_result(&dir).await {
        let total = read_manifest(&dir).await.map(|m| m.total_chunks).unwrap_or(0);
        return Ok(Json(ChunkResponse { received: total, total, complete: true, result: Some(result) }));
    }

    let manifest = read_manifest(&dir).await?;
    let meta = manifest
        .chunks
        .iter()
        .find(|c| c.index == index)
        .ok_or_else(|| AppError::InvalidArgument(format!("chunk index {index} out of range")))?;
    let expected = meta.end.saturating_sub(meta.start);
    if body.len() as u64 != expected {
        return Err(AppError::InvalidArgument(format!(
            "chunk {index} size {} != expected {expected}",
            body.len()
        ))
        .into());
    }

    let chunks_dir = dir.join("chunks");
    // Write to a temp name then rename so a partial write never looks "received".
    let tmp = chunks_dir.join(format!("{index}.part.tmp"));
    let final_path = chunks_dir.join(format!("{index}.part"));
    tokio::fs::write(&tmp, &body)
        .await
        .map_err(|e| AppError::Internal(format!("write chunk: {e}")))?;
    tokio::fs::rename(&tmp, &final_path)
        .await
        .map_err(|e| AppError::Internal(format!("commit chunk: {e}")))?;

    let received = received_chunks(&chunks_dir, manifest.total_chunks).await;
    let complete = received.len() as u32 == manifest.total_chunks;

    if complete {
        // Elect a single assembler via an atomic lock file; a racing duplicate
        // request returns "processing" (the client can poll status).
        match std::fs::OpenOptions::new().create_new(true).write(true).open(dir.join(".assembling")) {
            Ok(_) => {
                let result = reassemble_and_ingest(&state, &caller, &dir, &manifest).await?;
                let _ = tokio::fs::write(
                    dir.join("result.json"),
                    serde_json::to_vec(&result).unwrap_or_default(),
                )
                .await;
                // Free the (now-redundant) chunk bytes; keep manifest+result.
                let _ = tokio::fs::remove_dir_all(&chunks_dir).await;
                let _ = tokio::fs::remove_file(dir.join(".assembling")).await;
                return Ok(Json(ChunkResponse {
                    received: manifest.total_chunks,
                    total: manifest.total_chunks,
                    complete: true,
                    result: Some(result),
                }));
            }
            Err(_) => {
                // Someone else is assembling; result may already be ready.
                return Ok(Json(ChunkResponse {
                    received: manifest.total_chunks,
                    total: manifest.total_chunks,
                    complete: true,
                    result: read_result(&dir).await,
                }));
            }
        }
    }

    Ok(Json(ChunkResponse {
        received: received.len() as u32,
        total: manifest.total_chunks,
        complete: false,
        result: None,
    }))
}

/// Which chunks are present / missing, plus the result once complete.
async fn status(
    State(state): State<crate::rest::RestState>,
    Extension(caller): Extension<Identity>,
    AxPath(id): AxPath<String>,
) -> Result<Json<StatusResponse>, ApiError> {
    caller.require(PermissionLevel::Manager).map_err(ApiError::from)?;

    let id = sanitize_id(&id)?;
    let dir = upload_dir(&state, &id)?;
    let manifest = read_manifest(&dir).await?;
    let result = read_result(&dir).await;

    let received = if result.is_some() {
        (0..manifest.total_chunks).collect()
    } else {
        received_chunks(&dir.join("chunks"), manifest.total_chunks).await
    };
    let missing: Vec<u32> = (0..manifest.total_chunks).filter(|i| !received.contains(i)).collect();

    Ok(Json(StatusResponse {
        upload_id: id,
        filename: manifest.filename,
        total_chunks: manifest.total_chunks,
        chunk_size: manifest.chunk_size,
        total_size: manifest.total_size,
        complete: missing.is_empty(),
        received_chunks: received,
        missing_chunks: missing,
        result,
    }))
}

// ---------------------------------------------------------------------------
// Reassembly + ingest
// ---------------------------------------------------------------------------

async fn reassemble_and_ingest(
    state: &crate::rest::RestState,
    caller: &Identity,
    dir: &Path,
    manifest: &Manifest,
) -> Result<UploadResult, AppError> {
    let ingest = state
        .ingest
        .as_ref()
        .ok_or_else(|| AppError::Config("ingest service not configured".into()))?;
    let ingest_root = ingest.ingest_root.as_deref().unwrap_or(Path::new("."));

    // Stage the reassembled file under .tmp/<uuid>/source.<ext> (same layout
    // the multipart /upload uses), so the watcher ignores it and the sidecar
    // scan is isolated.
    let stage = ingest_root.join(".tmp").join(Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|e| AppError::Internal(format!("create stage dir: {e}")))?;
    let ext = Path::new(&manifest.filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let source = stage.join(format!("source.{ext}"));

    // Concatenate chunks in order, hashing as we go (never holds the whole file).
    let chunks_dir = dir.join("chunks");
    let mut hasher = Sha256::new();
    {
        let mut out = tokio::fs::File::create(&source)
            .await
            .map_err(|e| AppError::Internal(format!("create source: {e}")))?;
        use tokio::io::AsyncWriteExt;
        for i in 0..manifest.total_chunks {
            let bytes = tokio::fs::read(chunks_dir.join(format!("{i}.part")))
                .await
                .map_err(|e| AppError::Internal(format!("read chunk {i}: {e}")))?;
            hasher.update(&bytes);
            out.write_all(&bytes)
                .await
                .map_err(|e| AppError::Internal(format!("write source: {e}")))?;
        }
        out.flush().await.ok();
    }

    let digest = hasher.finalize();
    let got = hex_lower(&digest);
    if got != manifest.hash.to_ascii_lowercase() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
        // The assembled bytes don't match the declared hash — drop the whole
        // session so the client re-inits and re-uploads cleanly.
        let _ = tokio::fs::remove_dir_all(dir).await;
        warn!(upload_id = %manifest.upload_id, "reassembled hash mismatch");
        return Err(AppError::InvalidArgument("reassembled file hash mismatch".into()));
    }

    // Ingest: archive → multi-file, otherwise a single track.
    let result = if let Some(kind) = ArchiveKind::detect(Path::new(&manifest.filename)) {
        let res = ingest.organize_archive(caller, &source, kind).await;
        let _ = tokio::fs::remove_dir_all(&stage).await;
        let res = res?;
        UploadResult::Archive(ArchiveUploadResponse {
            kind: format!("{kind:?}"),
            ingested: res.ingested,
            already_indexed: res.already_indexed,
            non_audio_skipped: res.non_audio_skipped,
            errors: res.errors,
            track_ids: res.track_ids.iter().map(|id| id.to_string()).collect(),
        })
    } else {
        let res = ingest.organize_and_index(caller, &source).await;
        let _ = tokio::fs::remove_dir_all(&stage).await;
        let res = res?;
        UploadResult::Single(UploadResponse {
            track_id: res.track_id.to_string(),
            path: res.dest.to_string_lossy().into_owned(),
        })
    };

    debug!(upload_id = %manifest.upload_id, "chunked upload reassembled + ingested");
    Ok(result)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_pads_and_lowercases() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff, 0xa0]), "000fffa0");
    }

    #[test]
    fn sanitize_id_accepts_hex_rejects_traversal() {
        let h = "a".repeat(64);
        assert_eq!(sanitize_id(&h).unwrap(), h);
        // Mixed case is normalised to lowercase.
        assert_eq!(sanitize_id("ABCDEF0123456789").unwrap(), "abcdef0123456789");
        // Path traversal / separators / non-hex / wrong length are rejected.
        assert!(sanitize_id("../../etc/passwd").is_err());
        assert!(sanitize_id("abc/def0123456789").is_err());
        assert!(sanitize_id("nothex_zzzzzzzzzz").is_err());
        assert!(sanitize_id("short").is_err());
    }
}
