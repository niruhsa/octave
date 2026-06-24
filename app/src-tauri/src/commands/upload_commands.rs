//! Upload commands (Uploads v2 + background jobs).
//!
//! A pick (one file, an archive, a multi-selection, or a folder) becomes **one
//! upload session**. The job:
//!   * reads every source natively (Rust), computes each file's whole-file
//!     SHA-256 + a per-chunk SHA-256 map,
//!   * `init`s a single session declaring all files,
//!   * uploads each missing chunk (the server re-hashes + verifies it; a
//!     corrupt chunk is retried),
//!   * on the final chunk the server reassembles, verifies, ingests, and writes
//!     the report — which the job fetches and emits.
//!
//! Progress is surfaced three ways: `upload-progress` / `upload-complete` Tauri
//! events (the active uploader's own UI), a single OS notification, and the
//! server's live `uploads` broadcast (other users / admins via
//! [`uploads_subscribe`]).
//!
//! **File reading is always native (Rust), never the WebView.** Manager+ is
//! enforced server-side.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_fs::{FilePath, FsExt};
use tauri_plugin_notification::NotificationExt;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};
use crate::transport::{
    ChunkAck, ChunkInit, UploadFileInit, UploadInitRequest, UploadListFilter, UploadSummary,
    UploadView,
};

/// Audio extensions recognised by the server (matches `server/src/services/tag.rs::AUDIO_EXTS`).
const AUDIO_EXTS: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv", "aac", "mp4",
];

/// Chunk size for resumable uploads (128 KiB). Each chunk carries its own hash so
/// the server can verify it on arrival.
const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// How many times to re-send a chunk the server rejects (e.g. in-transit
/// corruption → hash mismatch) before giving up on the session.
const CHUNK_RETRIES: u32 = 3;

/// How many chunks to upload concurrently (in flight at once) within a file.
/// The server accepts concurrent chunks safely (per-chunk verify, atomic
/// received-count, single-finalizer election), so this just trades a little
/// memory (`CHUNK_CONCURRENCY × CHUNK_SIZE`) for throughput on high-latency
/// links.
const CHUNK_CONCURRENCY: usize = 4;

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
    upload_id: Option<String>,
    phase: String, // "scanning" | "uploading" | "finalizing" | "done"
    current: u64,  // current file index
    total: u64,    // total files in the session
    file: Option<String>,
    /// Bytes of the current file uploaded so far.
    received: Option<u64>,
    /// Size of the current file in bytes.
    bytes_total: Option<u64>,
    /// Bytes of the whole session uploaded so far.
    session_received: Option<u64>,
    /// Total bytes across the session.
    session_total: Option<u64>,
    /// Job-wide average upload speed (bytes/sec).
    bytes_per_sec: Option<f64>,
    ok: Option<bool>,
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompleteEvent {
    job_id: String,
    upload_id: Option<String>,
    /// Final session state: "completed" | "cancelled" | "error".
    state: String,
    total_files: u64,
    tracks_ingested: u64,
    files_failed: u64,
    skipped: u64,
    errors: Vec<String>,
}

// ── Filename / format helpers ────────────────────────────────────────────────

fn is_archive_name(filename: &str) -> bool {
    let name = filename.to_ascii_lowercase();
    name.ends_with(".tar.gz")
        || name.ends_with(".tgz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tbz2")
        || name.ends_with(".tbz")
        || name.ends_with(".tar.xz")
        || name.ends_with(".txz")
        || name.ends_with(".tar")
        || name.ends_with(".zip")
        || name.ends_with(".iso")
        || name.ends_with(".img")
        || name.ends_with(".nrg")
        || name.ends_with(".bin")
        || name.ends_with(".cue")
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['_', ' ', '.']).to_string();
    if trimmed.is_empty() {
        "upload".to_string()
    } else {
        trimmed
    }
}

/// True for a URI string (SAF `content://`, `file://`, …) vs a plain path.
fn is_uri(raw: &str) -> bool {
    raw.contains("://")
}

/// Best-effort display name for progress/notifications and as a filename hint.
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
        .or_else(|| {
            path.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        });

    if let Some(c) = &candidate {
        if is_uploadable(Path::new(c)) {
            return c.clone();
        }
    }

    let ext = sniff_ext(bytes).unwrap_or("bin");
    let raw_stem = candidate
        .as_deref()
        .map(|c| {
            Path::new(c)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(c)
                .to_string()
        })
        .unwrap_or_else(|| "upload".to_string());
    format!("{}.{ext}", sanitize_stem(&raw_stem))
}

/// Read a source's bytes **natively** (desktop path or Android `content://`
/// URI). Owned values keep the spawned job future `Send`.
async fn read_source(app: AppHandle, raw: String) -> Result<Vec<u8>, String> {
    let fp: FilePath = raw.parse().expect("FilePath::from_str is infallible");
    tokio::task::spawn_blocking(move || app.fs().read(fp))
        .await
        .map_err(|e| format!("task: {e}"))?
        .map_err(|e| e.to_string())
}

/// SHA-256 of bytes, lowercase hex.
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

/// Build a file's chunk map (with per-chunk hashes) + whole-file hash.
fn prepare_file(filename: String, data: &[u8]) -> UploadFileInit {
    let size = data.len() as u64;
    let total_chunks = size.div_ceil(CHUNK_SIZE).max(1);
    let mut chunks = Vec::with_capacity(total_chunks as usize);
    for i in 0..total_chunks {
        let start = i * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(size);
        chunks.push(ChunkInit {
            index: i as u32,
            start,
            end,
            hash: sha256_hex(&data[start as usize..end as usize]),
        });
    }
    UploadFileInit {
        filename,
        hash: sha256_hex(data),
        total_size: size,
        chunk_size: CHUNK_SIZE,
        total_chunks: total_chunks as u32,
        chunks,
    }
}

/// Has the session already reached a terminal `completed` state? Used to tell a
/// benign post-finalize chunk failure (lost ack + retry race) apart from a real
/// upload failure, which matters once chunks upload concurrently.
async fn session_completed(auth: &AuthManager, upload_id: &str) -> bool {
    auth.get_upload(upload_id.to_string())
        .await
        .map(|v| v.state == "completed")
        .unwrap_or(false)
}

/// Send one chunk, retrying a server-side rejection (corruption) a few times.
async fn put_chunk_with_retry(
    auth: &AuthManager,
    upload_id: &str,
    file_index: u32,
    chunk_index: u32,
    bytes: Vec<u8>,
) -> AppResult<ChunkAck> {
    let mut attempt = 0u32;
    loop {
        match auth
            .put_chunk(
                upload_id.to_string(),
                file_index,
                chunk_index,
                bytes.clone(),
            )
            .await
        {
            Ok(ack) => return Ok(ack),
            Err(e) => {
                attempt += 1;
                if attempt >= CHUNK_RETRIES {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(150 * attempt as u64)).await;
            }
        }
    }
}

// ── Notifications ────────────────────────────────────────────────────────────

const PROGRESS_CHANNEL: &str = "octave-upload-progress";
const COMPLETE_CHANNEL: &str = "octave-upload-complete";

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
        format!(
            "{bar}  {pct}%  ·  {}/{} files  ·  {speed}",
            file_no.min(total_files),
            total_files
        )
    } else {
        format!("{bar}  {pct}%  ·  {speed}")
    };
    notify_progress(app, id, "Uploading music", &body);
}

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

fn notify_complete(app: &AppHandle, progress_id: i32, title: &str, body: &str) {
    #[cfg(mobile)]
    let id = {
        let _ = app.notification().remove_active(vec![progress_id]);
        progress_id.wrapping_neg().wrapping_sub(1)
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
    let started = std::time::Instant::now();
    let total_items = items.len() as u64;

    // Phase 1: read every source natively + compute hashes/chunk maps.
    emit(
        &app,
        ProgressEvent {
            job_id: job_id.clone(),
            phase: "scanning".into(),
            total: total_items,
            ..Default::default()
        },
    );
    notify_progress(&app, notif_id, "Uploading music", "Preparing files…");

    let mut files_init: Vec<UploadFileInit> = Vec::new();
    let mut datas: Vec<(String, Vec<u8>)> = Vec::new(); // (hint, bytes), parallel to files_init
    let mut skipped: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for item in &items {
        let raw = item.path.clone();
        let hint = name_hint(&raw);
        let data = match read_source(app.clone(), raw.clone()).await {
            Ok(d) => d,
            Err(e) => {
                errors.push(format!("{hint}: read error: {e}"));
                continue;
            }
        };
        if data.is_empty() {
            skipped += 1;
            continue;
        }
        let filename = determine_filename(Some(&hint), &std::path::PathBuf::from(&raw), &data);
        files_init.push(prepare_file(filename, &data));
        datas.push((hint, data));
    }

    if files_init.is_empty() {
        emit_done(&app, &job_id, None, "completed", 0, 0, 0, skipped, errors);
        notify_complete(
            &app,
            notif_id,
            "Upload complete",
            &format!("0 uploaded · {skipped} skipped"),
        );
        return;
    }

    let total_files = files_init.len() as u64;
    let session_total: u64 = files_init.iter().map(|f| f.total_size).sum();

    // Phase 2: declare the session.
    let view = match auth
        .init_upload(UploadInitRequest {
            files: files_init.clone(),
        })
        .await
    {
        Ok(v) => v,
        Err(e) => {
            let msg = e.to_string();
            errors.push(msg.clone());
            emit_done(
                &app,
                &job_id,
                None,
                "error",
                total_files,
                0,
                total_files,
                skipped,
                errors,
            );
            notify_complete(&app, notif_id, "Upload failed", &msg);
            return;
        }
    };
    let upload_id = view.id.clone();

    // Phase 3: upload missing chunks (up to CHUNK_CONCURRENCY in flight).
    let mut sent_total: u64 = 0; // bytes transmitted this run (drives speed)
    let mut session_received: u64 = 0;
    let mut last_notif = std::time::Instant::now();
    let mut complete = false;

    'outer: for (fi, (hint, data)) in datas.into_iter().enumerate() {
        let file_init = &files_init[fi];
        // Chunks the server already holds (resume within a session).
        let already: std::collections::HashSet<u32> = view
            .files
            .get(fi)
            .map(|f| {
                f.chunks
                    .iter()
                    .filter(|c| c.received)
                    .map(|c| c.index)
                    .collect()
            })
            .unwrap_or_default();
        let mut file_received: u64 = 0;
        // Account already-uploaded chunks (resume) up-front.
        for chunk in &file_init.chunks {
            if already.contains(&chunk.index) {
                let len = chunk.end - chunk.start;
                file_received += len;
                session_received += len;
            }
        }

        // Upload the missing chunks concurrently. The network I/O is parallel
        // (`buffer_unordered`); the acks are consumed sequentially in this loop,
        // so the progress counters stay single-writer and need no locking. Each
        // chunk future owns its inputs (so the spawned job stays `Send + 'static`):
        // the file bytes are shared cheaply via an `Arc` and a chunk-sized copy
        // is sliced off inside the future, keeping at most `CHUNK_CONCURRENCY`
        // chunk buffers live at once.
        let data = std::sync::Arc::new(data);
        let missing: Vec<(u32, u64, u64)> = file_init
            .chunks
            .iter()
            .filter(|c| !already.contains(&c.index))
            .map(|c| (c.index, c.start, c.end))
            .collect();
        let mut chunks =
            futures_util::stream::iter(missing.into_iter().map(|(index, start, end)| {
                let auth = auth.clone();
                let upload_id = upload_id.clone();
                let data = data.clone();
                let len = end - start;
                async move {
                    let bytes = data[start as usize..end as usize].to_vec();
                    let res =
                        put_chunk_with_retry(&auth, &upload_id, fi as u32, index, bytes).await;
                    (len, res)
                }
            }))
            .buffer_unordered(CHUNK_CONCURRENCY);

        while let Some((len, res)) = chunks.next().await {
            match res {
                Ok(ack) => {
                    sent_total += len;
                    file_received += len;
                    session_received += len;
                    let speed = sent_total as f64 / started.elapsed().as_secs_f64().max(0.001);
                    emit(
                        &app,
                        ProgressEvent {
                            job_id: job_id.clone(),
                            upload_id: Some(upload_id.clone()),
                            phase: "uploading".into(),
                            current: fi as u64,
                            total: total_files,
                            file: Some(hint.clone()),
                            received: Some(file_received),
                            bytes_total: Some(file_init.total_size),
                            session_received: Some(session_received),
                            session_total: Some(session_total),
                            bytes_per_sec: Some(speed),
                            ..Default::default()
                        },
                    );
                    if last_notif.elapsed() >= Duration::from_millis(400) {
                        let frac = session_received as f64 / session_total.max(1) as f64;
                        notify_upload_progress(
                            &app,
                            notif_id,
                            frac,
                            fi as u64 + 1,
                            total_files,
                            speed,
                        );
                        last_notif = std::time::Instant::now();
                    }
                    if ack.upload_complete {
                        complete = true;
                        break;
                    }
                }
                Err(e) => {
                    // A chunk can fail *after* the session already finalized —
                    // e.g. a lost ack on a chunk the server did receive, then a
                    // retry races the now-Completed session (more likely with
                    // chunks in flight). Only fatal when the session isn't done.
                    if complete || session_completed(&auth, &upload_id).await {
                        complete = true;
                        break;
                    }
                    // Unrecoverable: cancel so the one-active-upload slot frees up.
                    errors.push(format!("{hint}: {e}"));
                    let _ = auth.cancel_upload(upload_id.clone()).await;
                    emit_done(
                        &app,
                        &job_id,
                        Some(&upload_id),
                        "cancelled",
                        total_files,
                        0,
                        total_files,
                        skipped,
                        errors,
                    );
                    notify_complete(&app, notif_id, "Upload failed", &e.to_string());
                    return;
                }
            }
        }
        // Drop the stream to cancel any still-in-flight chunk futures. When
        // `complete`, the server already received every chunk (that's why it
        // finalized), so abandoning their unread acks is safe.
        drop(chunks);
        if complete {
            break 'outer;
        }
    }

    // Phase 4: fetch the final report + announce.
    emit(
        &app,
        ProgressEvent {
            job_id: job_id.clone(),
            upload_id: Some(upload_id.clone()),
            phase: "finalizing".into(),
            current: total_files,
            total: total_files,
            session_received: Some(session_received),
            session_total: Some(session_total),
            ..Default::default()
        },
    );

    let report_view = auth.get_upload(upload_id.clone()).await.ok();
    let (tracks, files_failed, state) = match &report_view {
        Some(v) => (
            report_u64(v, "tracks_ingested"),
            report_u64(v, "files_failed"),
            v.state.clone(),
        ),
        None => (0, 0, "completed".to_string()),
    };
    emit_done(
        &app,
        &job_id,
        Some(&upload_id),
        &state,
        total_files,
        tracks,
        files_failed,
        skipped,
        errors,
    );

    let title = if files_failed > 0 {
        "Upload finished with errors"
    } else {
        "Upload complete"
    };
    let mut body = format!("{tracks} track(s) ingested");
    if files_failed > 0 {
        body.push_str(&format!(" · {files_failed} failed"));
    }
    if skipped > 0 {
        body.push_str(&format!(" · {skipped} skipped"));
    }
    notify_complete(&app, notif_id, title, &body);
}

fn report_u64(v: &UploadView, key: &str) -> u64 {
    v.report
        .as_ref()
        .and_then(|r| r.get(key))
        .and_then(|x| x.as_u64())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn emit_done(
    app: &AppHandle,
    job_id: &str,
    upload_id: Option<&str>,
    state: &str,
    total_files: u64,
    tracks_ingested: u64,
    files_failed: u64,
    skipped: u64,
    errors: Vec<String>,
) {
    let _ = app.emit(
        "upload-complete",
        CompleteEvent {
            job_id: job_id.to_string(),
            upload_id: upload_id.map(|s| s.to_string()),
            state: state.to_string(),
            total_files,
            tracks_ingested,
            files_failed,
            skipped,
            errors,
        },
    );
    let _ = app.emit(
        "upload-progress",
        ProgressEvent {
            job_id: job_id.to_string(),
            upload_id: upload_id.map(|s| s.to_string()),
            phase: "done".into(),
            current: total_files,
            total: total_files,
            ..Default::default()
        },
    );
}

fn emit(app: &AppHandle, ev: ProgressEvent) {
    let _ = app.emit("upload-progress", ev);
}

fn next_job() -> (String, i32) {
    let n = JOB_SEQ.fetch_add(1, Ordering::SeqCst);
    (format!("upload-{n}"), (n & 0x7fff_ffff) as i32)
}

async fn resolve_auth(
    state: &tauri::State<'_, crate::AppStateHandle>,
) -> AppResult<Arc<AuthManager>> {
    let guard = state.auth.read().await;
    guard
        .clone()
        .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Upload a set of picked files as a single session. Returns the job id;
/// progress arrives via `upload-progress` / `upload-complete` + a notification.
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

/// Upload every audio/archive file in a directory tree as a single session
/// (desktop). Returns the job id.
#[tauri::command]
pub async fn upload_folder(
    dir_path: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<String> {
    let auth = resolve_auth(&state).await?;
    let root = std::path::PathBuf::from(&dir_path);
    if !root.is_dir() {
        return Err(AppError::Internal(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    let (job_id, notif_id) = next_job();
    let jid = job_id.clone();
    tauri::async_runtime::spawn(async move {
        ensure_channels(&app);
        emit(
            &app,
            ProgressEvent {
                job_id: jid.clone(),
                phase: "scanning".into(),
                ..Default::default()
            },
        );
        notify_progress(&app, notif_id, "Uploading music", "Scanning folder…");

        let items: Vec<UploadItem> = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_uploadable(e.path()))
            .map(|e| UploadItem {
                path: e.path().to_string_lossy().into_owned(),
            })
            .collect();

        run_job(app, auth, jid, notif_id, items).await;
    });
    Ok(job_id)
}

/// List upload reports. Defaults to the caller's own; admins may filter by
/// `user_id`. `state` filters by lifecycle (`initialized`/`uploading`/
/// `completed`/`cancelled`).
#[tauri::command]
pub async fn uploads_list(
    filter: Option<UploadListFilter>,
    state: tauri::State<'_, crate::AppStateHandle>,
) -> AppResult<Vec<UploadSummary>> {
    let auth = resolve_auth(&state).await?;
    auth.list_uploads(filter.unwrap_or_default()).await
}

/// Fetch one upload report (per-file/per-chunk detail + completion report).
#[tauri::command]
pub async fn uploads_get(
    id: String,
    state: tauri::State<'_, crate::AppStateHandle>,
) -> AppResult<UploadView> {
    let auth = resolve_auth(&state).await?;
    auth.get_upload(id).await
}

/// Cancel an in-flight upload (owner or admin). Staged chunks are cleaned off
/// the server's disk.
#[tauri::command]
pub async fn uploads_cancel(
    id: String,
    state: tauri::State<'_, crate::AppStateHandle>,
) -> AppResult<UploadView> {
    let auth = resolve_auth(&state).await?;
    auth.cancel_upload(id).await
}

/// Subscribe to the live `uploads` channel (gRPC stream primary, WS fallback).
/// Spawns a reader that re-emits each event as a Tauri `upload-event`.
#[tauri::command]
pub async fn uploads_subscribe(
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<()> {
    let auth = resolve_auth(&state).await?;
    let mut rx = auth.subscribe_uploads().await?;
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app.emit("upload-event", ev);
        }
    });
    Ok(())
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
        assert_eq!(
            determine_filename(Some("Song.flac"), Path::new("/tmp/x"), b""),
            "Song.flac"
        );
        assert_eq!(
            determine_filename(Some("msf:13974"), Path::new("/tmp/x"), b"fLaC\0\0\0\0"),
            "msf_13974.flac"
        );
        assert_eq!(
            determine_filename(None, Path::new("/tmp/blob"), b"????"),
            "blob.bin"
        );
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
    fn prepare_file_builds_contiguous_chunk_map() {
        let data = vec![7u8; (CHUNK_SIZE + 100) as usize];
        let f = prepare_file("x.flac".into(), &data);
        assert_eq!(f.total_chunks, 2);
        assert_eq!(f.total_size, CHUNK_SIZE + 100);
        assert_eq!(f.chunks[0].start, 0);
        assert_eq!(f.chunks[0].end, CHUNK_SIZE);
        assert_eq!(f.chunks[1].start, CHUNK_SIZE);
        assert_eq!(f.chunks[1].end, CHUNK_SIZE + 100);
        // Each chunk hash is the sha256 of its slice.
        assert_eq!(f.chunks[1].hash, sha256_hex(&data[CHUNK_SIZE as usize..]));
        assert_eq!(f.hash, sha256_hex(&data));
    }

    #[test]
    fn name_hint_decodes_uris() {
        assert_eq!(name_hint("/music/song.flac"), "song.flac");
        assert_eq!(
            name_hint("content://media/documents/msf%3A13974"),
            "msf:13974"
        );
        assert_eq!(
            name_hint("content://x/y/album%20name.zip"),
            "album name.zip"
        );
        assert!(is_uri("content://x/y"));
        assert!(!is_uri("/a/b.flac"));
    }
}
