//! Upload commands (Phase 8 + background jobs).
//!
//! Uploads run as **background jobs**: `upload_files` / `upload_folder` resolve
//! auth, spawn a task on the async runtime, and return a `jobId` immediately so
//! the UI never blocks. The job:
//!   * emits `upload-progress` events (`{ jobId, phase, current, total,
//!     received, bytesTotal, … }`) — `received`/`bytesTotal` drive a live
//!     per-file byte bar (gRPC streams chunk-by-chunk; on REST they stay at 0
//!     so the UI shows an indeterminate "uploading" state),
//!   * drives a single OS notification that updates per file and is **replaced**
//!     by a completion notification at the end,
//!   * emits a final `upload-complete` event with the tally.
//!
//! **File reading is always native (Rust), never the WebView** — the job reads
//! each source via the fs plugin's Rust API (`app.fs().read`), which opens a
//! normal path on desktop and resolves a SAF `content://` URI through the
//! Android ContentResolver. Content-URI names are unreliable, so
//! [`name_hint`]/[`determine_filename`] percent-decode the URI tail, sniff the
//! real format from magic bytes, and sanitise the result.
//!
//! Manager+ is enforced server-side on every file.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_fs::{FilePath, FsExt};
use tauri_plugin_notification::NotificationExt;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};

/// Audio extensions recognised by the server (matches `server/src/services/tag.rs::AUDIO_EXTS`).
const AUDIO_EXTS: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv", "aac", "mp4",
];

/// Chunk size for resumable uploads (4 MiB). The server stores chunks keyed by
/// the file hash so an interrupted upload resumes (even from another device).
const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// Monotonic job id source (also seeds the per-job notification id).
static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

// ── Input / output types ─────────────────────────────────────────────────────

/// One source to upload — a desktop filesystem path or an Android `content://`
/// URI (read natively via the fs plugin).
#[derive(Debug, Clone, Deserialize)]
pub struct UploadItem {
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    job_id: String,
    phase: String, // "scanning" | "uploading" | "done"
    current: u64,
    total: u64,
    file: Option<String>,
    ok: Option<bool>,
    message: Option<String>,
    /// Bytes of the current file uploaded so far (chunk-granular).
    received: Option<u64>,
    /// Size of the current file in bytes.
    bytes_total: Option<u64>,
    /// Job-wide average upload speed (bytes/sec) so far.
    bytes_per_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompleteEvent {
    job_id: String,
    total: u64,
    succeeded: u64,
    failed: u64,
    skipped: u64,
    errors: Vec<String>,
}

// ── Filename / format helpers ────────────────────────────────────────────────

fn is_archive_name(filename: &str) -> bool {
    let name = filename.to_ascii_lowercase();
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
        || name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz")
        || name.ends_with(".tar.xz") || name.ends_with(".txz")
        || name.ends_with(".tar") || name.ends_with(".zip")
        || name.ends_with(".iso") || name.ends_with(".img")
        || name.ends_with(".nrg") || name.ends_with(".bin") || name.ends_with(".cue")
}

fn is_uploadable(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_ascii_lowercase());
    let Some(ref name) = name else { return false };
    if is_archive_name(name) {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Sniff a file format from its leading magic bytes. Fallback when a
/// (content-URI-derived) filename carries no usable extension.
fn sniff_ext(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 4 && &b[0..4] == b"fLaC" {
        return Some("flac");
    }
    if b.len() >= 4 && &b[0..4] == b"OggS" {
        return Some("ogg");
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        return Some("wav");
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return Some("m4a");
    }
    if b.len() >= 4 && &b[0..4] == b"FORM" {
        return Some("aiff");
    }
    if b.len() >= 4 && b[0..4] == [0x50, 0x4B, 0x03, 0x04] {
        return Some("zip");
    }
    if b.len() >= 2 && b[0..2] == [0x1F, 0x8B] {
        return Some("tar.gz");
    }
    if b.len() >= 3 && &b[0..3] == b"ID3" {
        return Some("mp3");
    }
    if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 {
        return Some("mp3");
    }
    None
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Minimal `%XX` percent-decoder for content-URI tails (e.g. `msf%3A13974`).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replace characters unsafe in a filename with `_`.
fn sanitize_stem(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches(['_', ' ', '.']).to_string();
    if trimmed.is_empty() { "upload".to_string() } else { trimmed }
}

/// True for a URI string (SAF `content://`, `file://`, …) vs a plain path.
fn is_uri(raw: &str) -> bool {
    raw.contains("://")
}

/// Best-effort display name for progress/notifications and as a filename hint.
/// Content-URI tails are percent-decoded (the real display name needs a native
/// ContentResolver query, which isn't available here — the UI falls back to
/// "File N of M" when this still looks like an opaque id).
fn name_hint(raw: &str) -> String {
    if is_uri(raw) {
        let tail = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
        let tail = tail.split('?').next().unwrap_or(tail);
        let decoded = percent_decode(tail);
        if !decoded.is_empty() {
            return decoded;
        }
        return "file".to_string();
    }
    Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}

/// Resolve the filename to send to the server: keep an explicit hint / the
/// path's own name when it already has a recognised extension; otherwise sniff
/// the format from the bytes and synthesise a sanitised `<stem>.<ext>`.
fn determine_filename(name: Option<&str>, path: &Path, bytes: &[u8]) -> String {
    let candidate = name
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));

    if let Some(c) = &candidate {
        if is_uploadable(Path::new(c)) {
            return c.clone();
        }
    }

    let ext = sniff_ext(bytes).unwrap_or("bin");
    let raw_stem = candidate
        .as_deref()
        .map(|c| Path::new(c).file_stem().and_then(|s| s.to_str()).unwrap_or(c).to_string())
        .unwrap_or_else(|| "upload".to_string());
    format!("{}.{ext}", sanitize_stem(&raw_stem))
}

/// Read a source's bytes **natively** (desktop path or Android `content://`
/// URI). Takes owned values (no borrowed args) so the enclosing spawned job
/// future stays `Send`. Runs on a blocking thread since the fs read is sync.
async fn read_source(app: AppHandle, raw: String) -> Result<Vec<u8>, String> {
    let fp: FilePath = raw.parse().expect("FilePath::from_str is infallible");
    tokio::task::spawn_blocking(move || app.fs().read(fp))
        .await
        .map_err(|e| format!("task: {e}"))?
        .map_err(|e| e.to_string())
}

/// Byte length of chunk `index` (the last chunk is short).
fn chunk_bytes(index: u64, file_size: u64) -> u64 {
    let start = index * CHUNK_SIZE;
    (start + CHUNK_SIZE).min(file_size) - start
}

/// Average bytes/sec given total bytes sent this session and the job start.
fn cur_speed(started: std::time::Instant, sent: u64) -> f64 {
    sent as f64 / started.elapsed().as_secs_f64().max(0.001)
}

/// Job-wide fraction done: completed files + the current file's fraction, over
/// the total file count (single file → just that file's fraction).
fn overall_frac(idx_u: u64, uploaded: u64, file_size: u64, total_files: u64) -> f64 {
    let file_frac = uploaded as f64 / file_size.max(1) as f64;
    ((idx_u as f64 + file_frac) / total_files.max(1) as f64).clamp(0.0, 1.0)
}

/// SHA-256 of the file bytes, lowercase hex — the upload id (server keys
/// resumable sessions by it, so the same file resumes across devices).
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Emit a per-file byte-progress event (chunk-granular) + job-wide speed.
fn emit_chunk_progress(
    app: &AppHandle,
    job_id: &str,
    idx_u: u64,
    total_files: u64,
    hint: &str,
    received: u64,
    file_size: u64,
    speed_bps: f64,
) {
    let _ = app.emit("upload-progress", ProgressEvent {
        job_id: job_id.to_string(),
        phase: "uploading".into(),
        current: idx_u,
        total: total_files,
        file: Some(hint.to_string()),
        received: Some(received),
        bytes_total: Some(file_size),
        bytes_per_sec: if speed_bps.is_finite() && speed_bps > 0.0 { Some(speed_bps) } else { None },
        ..Default::default()
    });
}

/// 10-cell text progress bar (the notification plugin has no native bar).
fn text_bar(pct: u32) -> String {
    let filled = (pct / 10).min(10) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

/// Human-readable speed, e.g. "4.2 MB/s".
fn format_speed(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "—".to_string();
    }
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut v = bps;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{:.0} {}", v, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

/// Update the silent progress notification with a text bar, overall %, files
/// count (for batches), and average speed.
fn notify_upload_progress(
    app: &AppHandle,
    id: i32,
    overall_frac: f64,
    file_no: u64,
    total_files: u64,
    speed_bps: f64,
) {
    let pct = ((overall_frac * 100.0).round().clamp(0.0, 100.0)) as u32;
    let speed = format_speed(speed_bps);
    let bar = text_bar(pct);
    let body = if total_files > 1 {
        format!("{bar}  {pct}%  ·  {}/{} files  ·  {speed}", file_no.min(total_files), total_files)
    } else {
        format!("{bar}  {pct}%  ·  {speed}")
    };
    notify_progress(app, id, "Uploading music", &body);
}

/// Upload one file via the chunked, resumable protocol: hash → init (learn
/// which chunks the server already has) → upload only the missing chunks →
/// the completing chunk (or a status poll) yields the ingest result. Owned
/// args keep the spawned job future `Send`.
#[allow(clippy::too_many_arguments)]
async fn upload_chunked(
    app: AppHandle,
    auth: Arc<AuthManager>,
    job_id: String,
    notif_id: i32,
    idx_u: u64,
    total_files: u64,
    hint: String,
    filename: String,
    data: Vec<u8>,
    started: std::time::Instant,
    sent_before: u64,
) -> AppResult<(crate::transport::UploadResult, u64)> {
    use crate::transport::{ChunkRange, UploadInitRequest};

    let file_size = data.len() as u64;
    let total_chunks = file_size.div_ceil(CHUNK_SIZE).max(1);
    let chunks: Vec<ChunkRange> = (0..total_chunks)
        .map(|i| {
            let start = i * CHUNK_SIZE;
            let end = (start + CHUNK_SIZE).min(file_size);
            ChunkRange { index: i as u32, start, end }
        })
        .collect();

    let init = auth
        .upload_init(UploadInitRequest {
            filename,
            hash: sha256_hex(&data),
            total_size: file_size,
            chunk_size: CHUNK_SIZE,
            total_chunks: total_chunks as u32,
            chunks,
        })
        .await?;
    let id = init.upload_id;
    let received: std::collections::HashSet<u32> = init.received_chunks.into_iter().collect();

    // `uploaded` = bytes of this file the server now has (resumed + sent this
    // session) → the progress bar; `sent` = bytes actually transmitted this
    // session → the average-speed numerator.
    let mut uploaded: u64 = received.iter().map(|&i| chunk_bytes(i as u64, file_size)).sum();
    let mut sent: u64 = 0;

    emit_chunk_progress(&app, &job_id, idx_u, total_files, &hint, uploaded, file_size, cur_speed(started, sent_before + sent));
    notify_upload_progress(&app, notif_id, overall_frac(idx_u, uploaded, file_size, total_files), idx_u + 1, total_files, cur_speed(started, sent_before + sent));
    let mut last_notif = std::time::Instant::now();

    let mut result: Option<crate::transport::UploadResult> = None;
    for i in 0..total_chunks {
        let index = i as u32;
        if received.contains(&index) {
            continue;
        }
        let sz = chunk_bytes(i, file_size);
        let start = (i * CHUNK_SIZE) as usize;
        let chunk = data[start..start + sz as usize].to_vec();
        let outcome = auth.upload_chunk(id.clone(), index, chunk).await?;
        uploaded += sz;
        sent += sz;
        if outcome.complete && outcome.result.is_some() {
            result = outcome.result;
        }
        let speed = cur_speed(started, sent_before + sent);
        emit_chunk_progress(&app, &job_id, idx_u, total_files, &hint, uploaded, file_size, speed);
        // The progress channel is silent, but throttle notification updates
        // anyway to avoid excessive IPC on fast links.
        if last_notif.elapsed() >= std::time::Duration::from_millis(400) {
            notify_upload_progress(&app, notif_id, overall_frac(idx_u, uploaded, file_size, total_files), idx_u + 1, total_files, speed);
            last_notif = std::time::Instant::now();
        }
    }
    // Always land a final tick for this file.
    notify_upload_progress(&app, notif_id, overall_frac(idx_u, uploaded, file_size, total_files), idx_u + 1, total_files, cur_speed(started, sent_before + sent));

    // Full resume (nothing to send) or a completing chunk that didn't carry the
    // result → fetch it from status.
    if result.is_none() {
        let st = auth.upload_status(id.clone()).await?;
        if st.result.is_some() {
            result = st.result;
        } else if st.complete {
            // All chunks present but not yet reassembled (a prior session was
            // interrupted between the final chunk write and reassembly) —
            // re-send the last chunk to trigger it.
            let last = total_chunks - 1;
            let sz = chunk_bytes(last, file_size);
            let start = (last * CHUNK_SIZE) as usize;
            let chunk = data[start..start + sz as usize].to_vec();
            result = auth.upload_chunk(id, last as u32, chunk).await?.result;
        }
    }

    let result =
        result.ok_or_else(|| AppError::Internal("upload finished but server returned no result".into()))?;
    Ok((result, sent_before + sent))
}

// ── Notifications ────────────────────────────────────────────────────────────
//
// Two Android channels: progress is **Low importance** (no sound/vibration/
// heads-up) so the per-file updates — same notification id, re-`show()`n — never
// re-alert; completion is **Default importance** and posted as a *new*
// notification id so it alerts exactly once. The progress builder is also
// `.silent()` + `.ongoing()`. On desktop the channel/silent/ongoing calls are
// no-ops, and completion just reuses the id (no vibration concern there).

const PROGRESS_CHANNEL: &str = "octave-upload-progress";
const COMPLETE_CHANNEL: &str = "octave-upload-complete";

/// Create the notification channels (idempotent). Android API 26+ requires a
/// channel before posting; importance is fixed at creation, which is exactly
/// why progress (Low, silent) and completion (Default, alerting) are separate.
#[cfg(mobile)]
fn ensure_channels(app: &AppHandle) {
    use tauri_plugin_notification::{Channel, Importance, Visibility};
    let _ = app.notification().create_channel(
        Channel::builder(PROGRESS_CHANNEL, "Upload progress")
            .description("Ongoing music upload progress")
            .importance(Importance::Low)
            .visibility(Visibility::Public)
            .vibration(false)
            .build(),
    );
    let _ = app.notification().create_channel(
        Channel::builder(COMPLETE_CHANNEL, "Upload complete")
            .description("Music upload finished")
            .importance(Importance::Default)
            .visibility(Visibility::Public)
            .build(),
    );
}
#[cfg(not(mobile))]
fn ensure_channels(_app: &AppHandle) {}

/// Update the single, silent progress notification (best-effort).
fn notify_progress(app: &AppHandle, id: i32, title: &str, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .id(id)
        .channel_id(PROGRESS_CHANNEL)
        .silent()
        .ongoing()
        .show();
}

/// Post the completion notification. On mobile this clears the ongoing progress
/// notification and posts a *new* id on the alerting channel (so it alerts
/// once); on desktop it reuses the id (replace).
fn notify_complete(app: &AppHandle, progress_id: i32, title: &str, body: &str) {
    #[cfg(mobile)]
    let id = {
        let _ = app.notification().remove_active(vec![progress_id]);
        progress_id.wrapping_neg().wrapping_sub(1) // distinct from any progress id
    };
    #[cfg(not(mobile))]
    let id = progress_id;

    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .id(id)
        .channel_id(COMPLETE_CHANNEL)
        .auto_cancel()
        .show();
}

// ── Job runner ───────────────────────────────────────────────────────────────

async fn run_job(
    app: AppHandle,
    auth: Arc<AuthManager>,
    job_id: String,
    notif_id: i32,
    items: Vec<UploadItem>,
) {
    ensure_channels(&app);
    let total = items.len() as u64;
    let started = std::time::Instant::now();
    let mut sent_total: u64 = 0; // bytes actually transmitted this session (for speed)
    let mut succeeded: u64 = 0;
    let mut failed: u64 = 0;
    let mut skipped: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        // Owned locals — nothing borrowed from `items` is held across an await,
        // which keeps the spawned job future `Send`.
        let raw = item.path.clone();
        let path_buf = std::path::PathBuf::from(&raw);
        let hint = name_hint(&raw);
        let idx_u = idx as u64;

        emit(&app, ProgressEvent {
            job_id: job_id.clone(),
            phase: "uploading".into(),
            current: idx_u,
            total,
            file: Some(hint.clone()),
            ..Default::default()
        });

        let data = match read_source(app.clone(), raw.clone()).await {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                errors.push(format!("{hint}: read error: {e}"));
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx_u + 1,
                    total,
                    file: Some(hint.clone()),
                    ok: Some(false),
                    message: Some(format!("read error: {e}")),
                    ..Default::default()
                });
                continue;
            }
        };

        if data.is_empty() {
            skipped += 1;
            continue;
        }

        let filename = determine_filename(Some(&hint), &path_buf, &data);

        match upload_chunked(
            app.clone(),
            auth.clone(),
            job_id.clone(),
            notif_id,
            idx_u,
            total,
            hint.clone(),
            filename,
            data,
            started,
            sent_total,
        )
        .await
        {
            Ok((_, sent_after)) => {
                sent_total = sent_after;
                succeeded += 1;
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx_u + 1,
                    total,
                    file: Some(hint.clone()),
                    ok: Some(true),
                    ..Default::default()
                });
            }
            Err(e) => {
                failed += 1;
                let msg = e.to_string();
                errors.push(format!("{hint}: {msg}"));
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx_u + 1,
                    total,
                    file: Some(hint.clone()),
                    ok: Some(false),
                    message: Some(msg),
                    ..Default::default()
                });
            }
        }
    }

    emit(&app, ProgressEvent {
        job_id: job_id.clone(),
        phase: "done".into(),
        current: total,
        total,
        ..Default::default()
    });

    let _ = app.emit("upload-complete", CompleteEvent {
        job_id: job_id.clone(),
        total,
        succeeded,
        failed,
        skipped,
        errors: errors.clone(),
    });

    let title = if failed > 0 { "Upload finished with errors" } else { "Upload complete" };
    let mut body = format!("{succeeded} uploaded");
    if failed > 0 {
        body.push_str(&format!(" · {failed} failed"));
    }
    if skipped > 0 {
        body.push_str(&format!(" · {skipped} skipped"));
    }
    notify_complete(&app, notif_id, title, &body);
}

fn emit(app: &AppHandle, ev: ProgressEvent) {
    let _ = app.emit("upload-progress", ev);
}

fn next_job() -> (String, i32) {
    let n = JOB_SEQ.fetch_add(1, Ordering::SeqCst);
    (format!("upload-{n}"), (n & 0x7fff_ffff) as i32)
}

async fn resolve_auth(state: &tauri::State<'_, crate::AppStateHandle>) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .clone()
        .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Upload a set of picked files in the background. Returns the job id; progress
/// arrives via `upload-progress` / `upload-complete` events + an OS
/// notification. `items[].path` may be a desktop path or an Android
/// `content://` URI — both are read natively (never through the WebView).
#[tauri::command]
pub async fn upload_files(
    items: Vec<UploadItem>,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<String> {
    let auth = resolve_auth(&state).await?;
    if items.is_empty() {
        return Err(AppError::Internal("no files selected".into()));
    }
    let (job_id, notif_id) = next_job();
    let jid = job_id.clone();
    tauri::async_runtime::spawn(async move {
        run_job(app, auth, jid, notif_id, items).await;
    });
    Ok(job_id)
}

/// Upload every audio/archive file in a directory tree in the background
/// (desktop — Android uses multi-file selection via `upload_files`). Returns the
/// job id; the scan + per-file progress arrive via events + a notification.
#[tauri::command]
pub async fn upload_folder(
    dir_path: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<String> {
    let auth = resolve_auth(&state).await?;
    let root = std::path::PathBuf::from(&dir_path);
    if !root.is_dir() {
        return Err(AppError::Internal(format!("not a directory: {}", root.display())));
    }
    let (job_id, notif_id) = next_job();
    let jid = job_id.clone();
    tauri::async_runtime::spawn(async move {
        ensure_channels(&app);
        emit(&app, ProgressEvent {
            job_id: jid.clone(),
            phase: "scanning".into(),
            ..Default::default()
        });
        notify_progress(&app, notif_id, "Uploading music", "Scanning folder…");

        let items: Vec<UploadItem> = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_uploadable(e.path()))
            .map(|e| UploadItem { path: e.path().to_string_lossy().into_owned() })
            .collect();

        run_job(app, auth, jid, notif_id, items).await;
    });
    Ok(job_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_detects_audio_magic() {
        assert_eq!(sniff_ext(b"fLaC\0\0\0"), Some("flac"));
        assert_eq!(sniff_ext(b"OggS....."), Some("ogg"));
        assert_eq!(sniff_ext(b"ID3\x03\x00"), Some("mp3"));
        assert_eq!(sniff_ext(b"RIFF\0\0\0\0WAVEfmt "), Some("wav"));
        assert_eq!(sniff_ext(b"\0\0\0\x18ftypM4A "), Some("m4a"));
        assert_eq!(sniff_ext(&[0x50, 0x4B, 0x03, 0x04, 0, 0]), Some("zip"));
        assert_eq!(sniff_ext(b"not audio"), None);
    }

    #[test]
    fn filename_decodes_sniffs_and_sanitises() {
        assert_eq!(determine_filename(Some("Song.flac"), Path::new("/tmp/x"), b""), "Song.flac");
        // Decoded opaque content-URI id + sniff → sanitised name + real ext.
        assert_eq!(
            determine_filename(Some("msf:13974"), Path::new("/tmp/x"), b"fLaC\0\0\0\0"),
            "msf_13974.flac"
        );
        assert_eq!(determine_filename(None, Path::new("/tmp/blob"), b"????"), "blob.bin");
    }

    #[test]
    fn text_bar_and_speed_format() {
        assert_eq!(text_bar(0), "░░░░░░░░░░");
        assert_eq!(text_bar(50), "█████░░░░░");
        assert_eq!(text_bar(100), "██████████");
        assert_eq!(format_speed(0.0), "—");
        assert_eq!(format_speed(512.0), "512 B/s");
        assert_eq!(format_speed(1024.0 * 1024.0 * 4.2), "4.2 MB/s");
    }

    #[test]
    fn overall_frac_blends_files_and_bytes() {
        // file 1 of 2, half done → 25% overall.
        assert!((overall_frac(0, 50, 100, 2) - 0.25).abs() < 1e-9);
        // last file fully done → 100%.
        assert!((overall_frac(1, 100, 100, 2) - 1.0).abs() < 1e-9);
        // single file, 30% → 0.30.
        assert!((overall_frac(0, 30, 100, 1) - 0.30).abs() < 1e-9);
    }

    #[test]
    fn chunk_bytes_handles_last_short_chunk() {
        let fs = CHUNK_SIZE * 2 + 123;
        assert_eq!(chunk_bytes(0, fs), CHUNK_SIZE);
        assert_eq!(chunk_bytes(1, fs), CHUNK_SIZE);
        assert_eq!(chunk_bytes(2, fs), 123); // last chunk is short
        assert_eq!(chunk_bytes(1, CHUNK_SIZE * 2), CHUNK_SIZE); // exact multiple
        assert_eq!(chunk_bytes(0, 10), 10); // smaller than one chunk
    }

    #[test]
    fn name_hint_decodes_uris() {
        assert_eq!(name_hint("/music/song.flac"), "song.flac");
        assert_eq!(name_hint("content://media/documents/msf%3A13974"), "msf:13974");
        assert_eq!(name_hint("content://x/y/album%20name.zip"), "album name.zip");
        assert!(is_uri("content://x/y"));
        assert!(!is_uri("/a/b.flac"));
    }
}
