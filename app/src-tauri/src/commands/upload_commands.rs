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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
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

/// Auto-pause threshold: if no chunk has succeeded for this long while the
/// upload is running, the job auto-pauses (state → paused). It keeps retrying;
/// a chunk landing again auto-resumes it. Replaces the old "give up + cancel
/// after 3 retries" behaviour so a network blip pauses instead of failing.
const STALL_THRESHOLD_MS: i64 = 60_000;

/// How often the stall monitor re-checks the threshold.
const STALL_CHECK_SECS: u64 = 5;

/// How many chunks to upload concurrently (in flight at once) within a file.
/// The server accepts concurrent chunks safely (per-chunk verify, atomic
/// received-count, single-finalizer election), so this just trades a little
/// memory (`CHUNK_CONCURRENCY × CHUNK_SIZE`) for throughput on high-latency
/// links.
const CHUNK_CONCURRENCY: usize = 4;

/// Monotonic job id source (also seeds the per-job notification id).
static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

// ── Pause / cancel control ────────────────────────────────────────────────────

/// Unix-epoch millis (best-effort; 0 on a pre-epoch clock).
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Per-job control shared between the running upload job (its chunk futures +
/// stall monitor) and the `uploads_pause` / `uploads_resume` / `uploads_cancel`
/// commands. One upload runs at a time, so a single slot suffices.
pub struct JobCtl {
    upload_id: String,
    job_id: String,
    notif_id: i32,
    /// Unix-millis of the last successful chunk; drives stall detection.
    last_success_ms: AtomicI64,
    /// User pressed pause — chunk futures park (send nothing) until resumed.
    manual_paused: AtomicBool,
    /// Auto-paused after a ≥`STALL_THRESHOLD_MS` stall; cleared on the next
    /// successful chunk (auto-resume).
    auto_paused: AtomicBool,
    /// Cancelled (locally via the command, or the session went terminal); the
    /// job stops promptly.
    cancelled: AtomicBool,
    /// True only during the chunk-upload phase — gates the stall monitor's life.
    active: AtomicBool,
}

impl JobCtl {
    fn new(upload_id: String, job_id: String, notif_id: i32) -> Self {
        Self {
            upload_id,
            job_id,
            notif_id,
            last_success_ms: AtomicI64::new(now_ms()),
            manual_paused: AtomicBool::new(false),
            auto_paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            active: AtomicBool::new(true),
        }
    }

    fn touch_success(&self) {
        self.last_success_ms.store(now_ms(), Ordering::Release);
    }

    fn stall_ms(&self) -> i64 {
        now_ms() - self.last_success_ms.load(Ordering::Acquire)
    }
}

/// Managed Tauri state holding the active upload job's control handle (if any).
#[derive(Default)]
pub struct UploadControl {
    slot: Mutex<Option<Arc<JobCtl>>>,
    /// Set while a resume job is verifying files / starting up, *before* it
    /// registers its `JobCtl` — so a second `uploads_resume_pending` (the resume
    /// trigger can re-fire) can't double-spawn during the verify window.
    resuming: AtomicBool,
}

impl UploadControl {
    fn set(&self, ctl: Arc<JobCtl>) {
        if let Ok(mut g) = self.slot.lock() {
            *g = Some(ctl);
        }
    }
    fn clear(&self) {
        if let Ok(mut g) = self.slot.lock() {
            *g = None;
        }
    }
    /// The active control handle iff it matches `upload_id`.
    fn get(&self, upload_id: &str) -> Option<Arc<JobCtl>> {
        self.slot
            .lock()
            .ok()
            .and_then(|g| g.as_ref().filter(|c| c.upload_id == upload_id).cloned())
    }
    /// Whether an upload job is currently registered (running).
    fn is_active(&self) -> bool {
        self.slot.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

/// Clears the `resuming` flag when the resume job ends (any exit path), so a
/// later resume can proceed.
struct ResumingGuard {
    app: AppHandle,
}

impl Drop for ResumingGuard {
    fn drop(&mut self) {
        self.app
            .state::<UploadControl>()
            .resuming
            .store(false, Ordering::Release);
    }
}

// ── Resume manifest (crash-safe local upload state) ───────────────────────────

/// Local resume state, persisted to app-private storage so an upload survives an
/// accidental process kill (e.g. the OS evicting the backgrounded app). Only the
/// minimum is stored — the server is the source of truth for the chunk map +
/// which chunks already landed (fetched via `get_upload` on resume); locally we
/// just need to know which **source file** maps to each `file_index` and its
/// whole-file hash so we can re-verify it hasn't changed. One upload runs at a
/// time, so a single manifest file suffices.
const RESUME_FILE: &str = "upload_resume.json";

/// App-private staging root. Picked Android `content://` URIs are **copied** here
/// at upload start so they survive a process kill (a SAF read grant doesn't) and
/// the upload reads chunks from these always-accessible files. Each session gets
/// a `<staging_root>/<staging_key>/` subdir, removed on every terminal outcome.
const STAGING_DIR: &str = "upload_staging";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResumeManifest {
    upload_id: String,
    job_id: String,
    /// Staging-subdir key (UUID) to clean up; `None` when nothing was staged
    /// (desktop reads original paths directly).
    #[serde(default)]
    staging_key: Option<String>,
    files: Vec<ResumeFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResumeFile {
    /// Effective source path to read on resume — a **staged copy** in app-private
    /// storage (for an Android `content://` source) or the original path (desktop).
    /// Either way it's a real, accessible filesystem path.
    path: String,
    name_hint: String,
    /// Whole-file SHA-256 captured when the upload started — re-checked on resume
    /// to detect a file whose contents changed since (→ cancel as corrupted).
    hash: String,
}

fn manifest_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(RESUME_FILE))
}

/// Persist the resume manifest atomically (temp + rename).
fn write_manifest(app: &AppHandle, m: &ResumeManifest) {
    let Some(path) = manifest_path(app) else {
        return;
    };
    let Ok(json) = serde_json::to_vec(m) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    } else {
        tracing::warn!("failed to persist upload resume manifest");
    }
}

fn read_manifest(app: &AppHandle) -> Option<ResumeManifest> {
    let path = manifest_path(app)?;
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn clear_manifest(app: &AppHandle) {
    if let Some(path) = manifest_path(app) {
        let _ = std::fs::remove_file(&path);
    }
}

// ── Staging (app-private copies of picked sources) ────────────────────────────

fn staging_root(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(STAGING_DIR))
}

fn staging_dir(app: &AppHandle, key: &str) -> Option<std::path::PathBuf> {
    staging_root(app).map(|r| r.join(key))
}

/// Copy a picked source's bytes into `<staging>/<key>/<index>.<ext>` and return
/// that path. The upload then reads chunks from here instead of the (revocable)
/// `content://` URI.
fn stage_file(
    app: &AppHandle,
    key: &str,
    index: usize,
    filename: &str,
    data: &[u8],
) -> Result<String, String> {
    let dir = staging_dir(app, key).ok_or_else(|| "no app data dir".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create staging dir: {e}"))?;
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let path = dir.join(format!("{index}.{ext}"));
    std::fs::write(&path, data).map_err(|e| format!("write staged file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

fn clear_staging(app: &AppHandle, key: &str) {
    if let Some(d) = staging_dir(app, key) {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Remove every staging subdir except `keep` (orphans from a crash before a
/// terminal cleanup). Called on startup so abandoned copies don't accumulate.
fn sweep_staging(app: &AppHandle, keep: Option<&str>) {
    let Some(root) = staging_root(app) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        if keep.map(|k| name.to_string_lossy() == k).unwrap_or(false) {
            continue;
        }
        let _ = std::fs::remove_dir_all(e.path());
    }
}

/// Clear all local resume state for the active upload: its staging copies (from
/// the manifest's `staging_key`) + the manifest itself. Called on every terminal
/// outcome (complete / cancel / corrupted).
fn clear_resume_state(app: &AppHandle) {
    if let Some(key) = read_manifest(app).and_then(|m| m.staging_key) {
        clear_staging(app, &key);
    }
    clear_manifest(app);
}

/// Read `[start, end)` of a staged/source file on a blocking thread — how chunks
/// are pulled for upload, so whole files never sit in memory.
async fn read_range(path: &str, start: u64, end: u64) -> Result<Vec<u8>, String> {
    let path = path.to_string();
    let path_for_err = path.clone();
    let len = (end.saturating_sub(start)) as usize;
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf)?;
        Ok(buf)
    })
    .await
    .map_err(|e| format!("read task: {e}"))?
    .map_err(|e| format!("{path_for_err}: {e}"))
}

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
    /// Pause-state transitions: `Some(true)` = paused, `Some(false)` = resumed,
    /// `None` = unchanged (a normal progress tick). Drives the UI pause badge.
    paused: Option<bool>,
    /// Why it paused: "manual" | "stalled". Only set alongside `paused: true`.
    pause_reason: Option<String>,
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

/// Lowercase-hex encode a digest (or any bytes).
fn hex_digest(out: impl AsRef<[u8]>) -> String {
    let out = out.as_ref();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SHA-256 of bytes, lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex_digest(h.finalize())
}

/// Build a file's chunk map (per-chunk hashes) + whole-file hash in a **single
/// pass** over `data`, invoking `on_progress(bytes_hashed, total)` after each
/// chunk. The whole-file digest is folded in incrementally (rather than a second
/// `sha256_hex(data)` pass), and the callback lets the caller surface per-file
/// hashing progress — hashing a large album file is otherwise a long, silent
/// stall with no UI movement.
fn prepare_file(
    filename: String,
    data: &[u8],
    mut on_progress: impl FnMut(u64, u64),
) -> UploadFileInit {
    use sha2::{Digest, Sha256};
    let size = data.len() as u64;
    let total_chunks = size.div_ceil(CHUNK_SIZE).max(1);
    let mut chunks = Vec::with_capacity(total_chunks as usize);
    let mut whole = Sha256::new();
    for i in 0..total_chunks {
        let start = i * CHUNK_SIZE;
        let end = (start + CHUNK_SIZE).min(size);
        let slice = &data[start as usize..end as usize];
        whole.update(slice);
        chunks.push(ChunkInit {
            index: i as u32,
            start,
            end,
            hash: sha256_hex(slice),
        });
        on_progress(end, size);
    }
    UploadFileInit {
        filename,
        hash: hex_digest(whole.finalize()),
        total_size: size,
        chunk_size: CHUNK_SIZE,
        total_chunks: total_chunks as u32,
        chunks,
    }
}

/// The server's current state string for a session, or `None` if unreachable.
async fn session_state(auth: &AuthManager, upload_id: &str) -> Option<String> {
    auth.get_upload(upload_id.to_string())
        .await
        .ok()
        .map(|v| v.state)
}

/// Outcome of a resilient chunk send.
enum ChunkSend {
    /// Server accepted (+ verified) the chunk.
    Ok(ChunkAck),
    /// The job was cancelled (locally, or the session went terminal-cancelled).
    Cancelled,
    /// The session is already `completed` (benign post-finalize lost-ack race).
    AlreadyComplete,
    /// The local staged/source file couldn't be read (vanished / disk error) —
    /// fatal for this upload.
    ReadError(String),
}

/// Send one chunk, retrying transient failures **indefinitely** with capped
/// exponential backoff — a network blip then pauses (via the stall monitor)
/// rather than failing the upload. Behaviours:
///   * **cancel**: returns `Cancelled` promptly when `ctl.cancelled` is set.
///   * **manual pause**: parks (sends nothing) while `ctl.manual_paused`.
///   * **success**: stamps `last_success_ms` (feeds stall detection + resume).
///   * **terminal session**: every few failures it polls the session; a
///     `completed`/`cancelled` server state ends the retry loop cleanly.
async fn send_chunk_resilient(
    auth: &AuthManager,
    ctl: &JobCtl,
    file_index: u32,
    chunk_index: u32,
    bytes: Vec<u8>,
) -> ChunkSend {
    let mut backoff = Duration::from_millis(500);
    let mut fails: u32 = 0;
    loop {
        if ctl.cancelled.load(Ordering::Acquire) {
            return ChunkSend::Cancelled;
        }
        // Park while manually paused (poll — cheap when idle, ~250 ms to resume).
        while ctl.manual_paused.load(Ordering::Acquire) && !ctl.cancelled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if ctl.cancelled.load(Ordering::Acquire) {
            return ChunkSend::Cancelled;
        }

        match auth
            .put_chunk(
                ctl.upload_id.clone(),
                file_index,
                chunk_index,
                bytes.clone(),
            )
            .await
        {
            Ok(ack) => {
                ctl.touch_success();
                return ChunkSend::Ok(ack);
            }
            Err(_e) => {
                fails += 1;
                // Don't retry forever against a dead session: poll its state
                // periodically (cheap relative to the backoff; skipped while the
                // server is unreachable since the poll just returns None).
                if fails.is_multiple_of(4) {
                    match session_state(auth, &ctl.upload_id).await.as_deref() {
                        Some("completed") => return ChunkSend::AlreadyComplete,
                        Some("cancelled") => {
                            ctl.cancelled.store(true, Ordering::Release);
                            return ChunkSend::Cancelled;
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
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
    notify_progress(app, id, "Uploading music", &body, pct as i32);
}

/// Post / update the in-progress upload notification.
///
/// On **Android** this drives the foreground-service notification ([`UploadService`]):
/// the service — not this notification — is what actually keeps the upload alive
/// in the background, so the text/progress is pushed through it. On desktop /
/// iOS there is no foreground service, so it falls back to a plain `ongoing`
/// notification. `progress` is `0..=100` (determinate) or `< 0` (indeterminate).
fn notify_progress(app: &AppHandle, id: i32, title: &str, body: &str, progress: i32) {
    #[cfg(target_os = "android")]
    {
        let _ = id;
        crate::upload_session::update(app, title, body, progress);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = progress;
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
}

/// RAII guard that stops the Android upload foreground service (releasing its
/// wake / WiFi locks + persistent notification) when the job ends — on **any**
/// exit path, including an early `return` or a panic unwind. No-op on desktop.
struct UploadForegroundGuard {
    app: AppHandle,
}

impl Drop for UploadForegroundGuard {
    fn drop(&mut self) {
        crate::upload_session::stop(&self.app);
    }
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

    // Android: bring up the upload foreground service **first thing**, while the
    // app is still foreground (the user just tapped upload — Android 12+ forbids
    // starting a foreground service from the background). It keeps the process +
    // network alive (persistent notification + wake/WiFi locks) so the upload
    // survives the app being backgrounded or the screen locking. The guard stops
    // it on every exit path below. No-op on desktop.
    crate::upload_session::start(&app, "Uploading music", "Preparing files…", -1);
    let _fg = UploadForegroundGuard { app: app.clone() };

    let started = std::time::Instant::now();
    let total_items = items.len() as u64;
    // Unique staging key for this session's app-private source copies (Android).
    // Cleaned up on any terminal outcome; orphans are swept on next startup.
    let staging_key = uuid::Uuid::new_v4().to_string();

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
    notify_progress(&app, notif_id, "Uploading music", "Preparing files…", -1);

    let mut files_init: Vec<UploadFileInit> = Vec::new();
    // (hint, effective source path), parallel to files_init. Chunks are read from
    // these paths at upload time — a staged copy (content://) or the original.
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut manifest_files: Vec<ResumeFile> = Vec::new(); // parallel to files_init (resume)
    let mut staged_any = false;
    let mut skipped: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let raw = item.path.clone();
        let hint = name_hint(&raw);
        let fileno = idx as u64 + 1;

        // Stage: reading the source bytes (native — slow for large files /
        // Android `content://` URIs). The read itself runs on a blocking thread,
        // so this status reaches the UI before the read starts.
        prepare_status(
            &app,
            &job_id,
            notif_id,
            idx as u64,
            total_items,
            &hint,
            &format!("Reading file {fileno}/{total_items}"),
        );
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

        // Stage: hashing + chunking (single pass over the bytes). Post the
        // per-file stage to both the UI and the notification, then stream
        // throttled intra-file percent to the UI so a large file shows movement
        // instead of stalling silently.
        prepare_status(
            &app,
            &job_id,
            notif_id,
            idx as u64,
            total_items,
            &hint,
            &format!("Hashing file {fileno}/{total_items}"),
        );
        let filename = determine_filename(Some(&hint), &std::path::PathBuf::from(&raw), &data);
        let app2 = app.clone();
        let job2 = job_id.clone();
        let hint2 = hint.clone();
        let mut last = std::time::Instant::now();
        let file_init = prepare_file(filename, &data, |done, total| {
            if last.elapsed() >= Duration::from_millis(150) {
                let pct = done.saturating_mul(100).checked_div(total).unwrap_or(100);
                emit_prepare(
                    &app2,
                    &job2,
                    idx as u64,
                    total_items,
                    &hint2,
                    &format!("Hashing file {fileno}/{total_items} · {pct}%"),
                );
                last = std::time::Instant::now();
            }
        });
        // Resolve the effective source path. An Android `content://` URI is
        // **copied** into app-private staging now (while the SAF read grant is
        // still valid) so a later resume — after a force-quit dropped the grant —
        // reads an always-accessible file. Desktop paths persist, so use them
        // directly. Either way, chunks stream from this path (no whole file in RAM).
        let effective_path = if is_uri(&raw) {
            match stage_file(&app, &staging_key, idx, &file_init.filename, &data) {
                Ok(p) => {
                    staged_any = true;
                    p
                }
                Err(e) => {
                    errors.push(format!("{hint}: {e}"));
                    continue;
                }
            }
        } else {
            raw.clone()
        };
        manifest_files.push(ResumeFile {
            path: effective_path.clone(),
            name_hint: hint.clone(),
            hash: file_init.hash.clone(),
        });
        files_init.push(file_init);
        sources.push((hint, effective_path));
        // `data` is dropped here — chunks are re-read from `effective_path`.
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

    // Bridge the gap between hashing the last file and the first chunk landing
    // (the session-declare round-trip), so the UI doesn't freeze on the last
    // "Hashing …" line.
    prepare_status(
        &app,
        &job_id,
        notif_id,
        total_files,
        total_files,
        "",
        "Starting upload…",
    );

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

    // Persist a resume manifest so an accidental process kill can resume this
    // upload — without re-uploading already-sent chunks, or re-hashing then
    // starting over. Cleared once the upload reaches a terminal state (in
    // `drive_upload`). The server owns the chunk map + received-set (fetched via
    // `get_upload` on resume); locally we keep only the source→file_index mapping
    // + each file's whole-file hash (re-checked on resume to detect a change).
    write_manifest(
        &app,
        &ResumeManifest {
            upload_id: upload_id.clone(),
            job_id: job_id.clone(),
            staging_key: staged_any.then(|| staging_key.clone()),
            files: manifest_files,
        },
    );

    drive_upload(
        app, auth, job_id, notif_id, view, sources, started, skipped, errors,
    )
    .await;
}

/// Drive the chunk-upload phase of a session to completion: register pause/cancel
/// control + the stall monitor, upload every missing chunk (resilient send),
/// then fetch + announce the report. Shared by a fresh upload ([`run_job`]) and
/// a resumed one ([`run_resume_job`]) — both arrive here with a server
/// [`UploadView`] (authoritative chunk map + received-set) and the effective
/// source paths (`sources`, indexed by `file_index`) that chunks are **read from
/// disk** as they're sent (so whole files never sit in RAM). Clears all resume
/// state (manifest + staged copies) on every terminal exit. The caller owns the
/// foreground-service guard (kept alive across this call).
#[allow(clippy::too_many_arguments)]
async fn drive_upload(
    app: AppHandle,
    auth: Arc<AuthManager>,
    job_id: String,
    notif_id: i32,
    view: UploadView,
    sources: Vec<(String, String)>,
    started: std::time::Instant,
    skipped: u64,
    mut errors: Vec<String>,
) {
    let upload_id = view.id.clone();
    let total_files = view.files.len() as u64;
    let session_total: u64 = view.files.iter().map(|f| f.total_size).sum();

    // Register pause/cancel control for this session + start the stall monitor.
    // One upload runs at a time, so a single control slot suffices. The guard
    // clears the slot + stops the monitor on every exit path below.
    let ctl = Arc::new(JobCtl::new(upload_id.clone(), job_id.clone(), notif_id));
    app.state::<UploadControl>().set(ctl.clone());
    let _ctl_guard = JobCtlGuard {
        app: app.clone(),
        ctl: ctl.clone(),
    };
    spawn_stall_monitor(app.clone(), auth.clone(), ctl.clone());

    // If we're picking up a session the server already marked `paused` — typically
    // its stall sweeper paused it while this client was force-quit, and we're now
    // resuming — flip it back to `uploading`. A fresh `JobCtl` has `auto_paused =
    // false`, so the Ok-arm's auto-resume wouldn't fire on its own; resume it
    // explicitly now (best-effort), and if that call can't land (offline at
    // resume start) arm `auto_paused` so the first successful chunk re-asserts it.
    if view.state == "paused" {
        match auth.resume_upload(upload_id.clone()).await {
            Ok(_) => emit_resumed(&app, &job_id, &upload_id),
            Err(_) => ctl.auto_paused.store(true, Ordering::Release),
        }
    }

    // Phase 3: upload missing chunks (up to CHUNK_CONCURRENCY in flight). A chunk
    // send now retries transient failures **indefinitely** (a ≥60s stall
    // auto-pauses instead of failing), parks while manually paused, and only
    // stops on cancel or a terminal session state.
    let mut sent_total: u64 = 0; // bytes transmitted this run (drives speed)
    let mut session_received: u64 = 0;
    let mut last_notif = std::time::Instant::now();
    let mut complete = false;
    let mut cancelled = false;
    let mut read_error: Option<String> = None;

    'outer: for (fi, (hint, path)) in sources.into_iter().enumerate() {
        // Authoritative chunk map + received-set from the server view — works
        // for a fresh session (nothing received) and a resumed one (some
        // already received, which we skip).
        let Some(file_view) = view.files.iter().find(|f| f.file_index == fi as u32) else {
            continue;
        };
        let already: std::collections::HashSet<u32> = file_view
            .chunks
            .iter()
            .filter(|c| c.received)
            .map(|c| c.index)
            .collect();
        let mut file_received: u64 = 0;
        // Account already-uploaded chunks (resume) up-front.
        for chunk in &file_view.chunks {
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
        // the source path is shared cheaply via an `Arc`, the chunk bytes are read
        // from disk inside the future (≤ `CHUNK_CONCURRENCY × CHUNK_SIZE` live at
        // once), and the per-job `JobCtl` is `Arc`-shared.
        let path = std::sync::Arc::new(path);
        let missing: Vec<(u32, u64, u64)> = file_view
            .chunks
            .iter()
            .filter(|c| !already.contains(&c.index))
            .map(|c| (c.index, c.start, c.end))
            .collect();
        let mut chunks =
            futures_util::stream::iter(missing.into_iter().map(|(index, start, end)| {
                let auth = auth.clone();
                let ctl = ctl.clone();
                let path = path.clone();
                let len = end - start;
                async move {
                    let res = match read_range(&path, start, end).await {
                        Ok(bytes) => {
                            send_chunk_resilient(&auth, &ctl, fi as u32, index, bytes).await
                        }
                        Err(e) => ChunkSend::ReadError(e),
                    };
                    (len, res)
                }
            }))
            .buffer_unordered(CHUNK_CONCURRENCY);

        while let Some((len, res)) = chunks.next().await {
            match res {
                ChunkSend::Ok(ack) => {
                    // A chunk landing after an auto-pause resumes the upload.
                    if ctl.auto_paused.swap(false, Ordering::AcqRel) {
                        let _ = auth.resume_upload(upload_id.clone()).await;
                        emit_resumed(&app, &job_id, &upload_id);
                    }
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
                            bytes_total: Some(file_view.total_size),
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
                ChunkSend::AlreadyComplete => {
                    // Benign post-finalize lost-ack race: the server already
                    // received every chunk (that's why it finalized).
                    complete = true;
                    break;
                }
                ChunkSend::Cancelled => {
                    cancelled = true;
                    break;
                }
                ChunkSend::ReadError(e) => {
                    read_error = Some(e);
                    break;
                }
            }
        }
        // Drop the stream to cancel any still-in-flight chunk futures.
        drop(chunks);
        if complete || cancelled || read_error.is_some() {
            break 'outer;
        }
    }

    // A cancel (local or remote) ends the job here — there's no report to fetch.
    // The server is already cancelled (by the command or remotely), so we don't
    // re-cancel; just announce.
    if cancelled {
        clear_resume_state(&app); // terminal — don't resume a cancelled upload
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
        notify_complete(
            &app,
            notif_id,
            "Upload cancelled",
            "The upload was cancelled.",
        );
        return;
    }

    // A local read failure (the staged/source file vanished mid-upload) is fatal:
    // cancel the session + clear resume state so we don't loop on a dead file.
    if let Some(e) = read_error {
        let _ = auth.cancel_upload(upload_id.clone()).await;
        clear_resume_state(&app);
        errors.push(format!("Could not read a file to upload — {e}"));
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
        notify_complete(
            &app,
            notif_id,
            "Upload failed",
            "Could not read a file from disk.",
        );
        return;
    }

    // Chunk phase over — stop the stall monitor so the (quick) report fetch
    // below can't trip an auto-pause. The guard also clears it on return.
    ctl.active.store(false, Ordering::Release);

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
    notify_progress(&app, notif_id, "Uploading music", "Finalizing…", -1);

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
    clear_resume_state(&app); // terminal — upload finished
}

/// Resume an upload persisted by a previous app session (its resume manifest).
///
/// 1. **Re-verify** every source file's whole-file SHA-256 against the manifest.
///    If **any** file changed (or is no longer readable), the whole session is
///    **cancelled** as corrupted with a clear reason — we never upload mismatched
///    bytes onto a half-finished session.
/// 2. **Fetch** the server view (authoritative chunk map + which chunks already
///    landed). A transient/offline failure keeps the manifest for a later launch;
///    a gone/forbidden session drops it.
/// 3. **Drive** only the missing chunks (shared [`drive_upload`]) — no re-upload
///    of already-sent chunks, no re-hash-then-start-over.
async fn run_resume_job(app: AppHandle, auth: Arc<AuthManager>, manifest: ResumeManifest) {
    // Releases the `resuming` flag on every exit (incl. the early corrupted /
    // unreachable returns, before `drive_upload` registers the real `JobCtl`).
    let _resuming_guard = ResumingGuard { app: app.clone() };
    ensure_channels(&app);
    crate::upload_session::start(&app, "Resuming upload", "Checking files…", -1);
    let _fg = UploadForegroundGuard { app: app.clone() };

    let (job_id, notif_id) = next_job();
    let started = std::time::Instant::now();
    let total = manifest.files.len() as u64;
    let upload_id = manifest.upload_id.clone();

    // Phase 1: re-read + re-hash each file (the staged copy on Android, or the
    // original path on desktop), verifying it hasn't changed since the upload
    // started. We pass the path (not the bytes) on to `drive_upload`, which reads
    // chunks from disk.
    let mut sources: Vec<(String, String)> = Vec::with_capacity(manifest.files.len());
    let mut corrupted: Vec<String> = Vec::new();
    for (idx, f) in manifest.files.iter().enumerate() {
        let fileno = idx as u64 + 1;
        prepare_status(
            &app,
            &job_id,
            notif_id,
            idx as u64,
            total,
            &f.name_hint,
            &format!("Verifying file {fileno}/{total}"),
        );
        match read_source(app.clone(), f.path.clone()).await {
            Ok(bytes) if sha256_hex(&bytes) == f.hash => {
                sources.push((f.name_hint.clone(), f.path.clone()))
            }
            Ok(_) => corrupted.push(format!(
                "{}: contents changed since the upload started",
                f.name_hint
            )),
            Err(e) => corrupted.push(format!("{}: no longer readable ({e})", f.name_hint)),
        }
    }

    // Any changed/unreadable file → cancel the whole session as corrupted.
    if !corrupted.is_empty() {
        let _ = auth.cancel_upload(upload_id.clone()).await;
        clear_resume_state(&app);
        let reason = format!(
            "File(s) changed/corrupted since the upload started — {}",
            corrupted.join("; ")
        );
        emit_done(
            &app,
            &job_id,
            Some(&upload_id),
            "cancelled",
            total,
            0,
            total,
            0,
            vec![reason.clone()],
        );
        notify_complete(&app, notif_id, "Upload cancelled — file changed", &reason);
        return;
    }

    // Phase 2: fetch the authoritative server view (chunk map + received-set).
    let view = match auth.get_upload(upload_id.clone()).await {
        // Already terminal server-side — nothing to resume.
        Ok(v) if v.state == "completed" || v.state == "cancelled" => {
            clear_resume_state(&app);
            emit_done(
                &app,
                &job_id,
                Some(&upload_id),
                &v.state,
                total,
                0,
                0,
                0,
                vec![],
            );
            return;
        }
        Ok(v) => v,
        Err(e) => {
            // Keep the manifest only for a transient/offline failure (retry on a
            // later launch); a gone/forbidden session drops it so we don't spin.
            let keep = matches!(e, AppError::Transport(_));
            if !keep {
                clear_resume_state(&app);
            }
            tracing::warn!(upload_id = %upload_id, keep, error = %e, "resume: could not fetch session");
            emit_done(
                &app,
                &job_id,
                Some(&upload_id),
                "error",
                total,
                0,
                0,
                0,
                vec![format!("Couldn't resume upload: {e}")],
            );
            return;
        }
    };

    // Phase 3+4: drive only the remaining chunks (shared with a fresh upload).
    drive_upload(
        app,
        auth,
        job_id,
        notif_id,
        view,
        sources,
        started,
        0,
        vec![],
    )
    .await;
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

/// Emit a pause-state transition (paused / resumed) for the UI badge. These
/// carry only the pause flag — the store overlays them onto the live progress
/// without clobbering the byte counters.
fn emit_paused(app: &AppHandle, job_id: &str, upload_id: &str, reason: &str) {
    emit(
        app,
        ProgressEvent {
            job_id: job_id.to_string(),
            upload_id: Some(upload_id.to_string()),
            phase: "uploading".into(),
            paused: Some(true),
            pause_reason: Some(reason.to_string()),
            ..Default::default()
        },
    );
}

fn emit_resumed(app: &AppHandle, job_id: &str, upload_id: &str) {
    emit(
        app,
        ProgressEvent {
            job_id: job_id.to_string(),
            upload_id: Some(upload_id.to_string()),
            phase: "uploading".into(),
            paused: Some(false),
            ..Default::default()
        },
    );
}

/// Watch the active job for a chunk stall: if no chunk has succeeded for
/// `STALL_THRESHOLD_MS` while running and not already paused, auto-pause it
/// (best-effort server pause + UI badge + notification). Auto-resume is handled
/// on the next successful chunk (in the driving loop). Exits when the job's
/// chunk phase ends or it's cancelled.
fn spawn_stall_monitor(app: AppHandle, auth: Arc<AuthManager>, ctl: Arc<JobCtl>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(STALL_CHECK_SECS)).await;
            if !ctl.active.load(Ordering::Acquire) || ctl.cancelled.load(Ordering::Acquire) {
                return;
            }
            // Already paused (manually or auto) → nothing to do.
            if ctl.manual_paused.load(Ordering::Acquire) || ctl.auto_paused.load(Ordering::Acquire)
            {
                continue;
            }
            if ctl.stall_ms() >= STALL_THRESHOLD_MS
                && ctl
                    .auto_paused
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                tracing::warn!(
                    upload_id = %ctl.upload_id,
                    "upload stalled ≥{}s — auto-pausing",
                    STALL_THRESHOLD_MS / 1000
                );
                // Best-effort: the server is often *why* we stalled (unreachable),
                // so this may fail — the local paused state still surfaces.
                let _ = auth.pause_upload(ctl.upload_id.clone()).await;
                emit_paused(&app, &ctl.job_id, &ctl.upload_id, "stalled");
                notify_progress(
                    &app,
                    ctl.notif_id,
                    "Upload paused",
                    "Stalled — waiting for a connection…",
                    -1,
                );
            }
        }
    });
}

/// Clears the active [`UploadControl`] slot + stops the stall monitor when the
/// chunk phase ends (any exit path, including a panic unwind).
struct JobCtlGuard {
    app: AppHandle,
    ctl: Arc<JobCtl>,
}

impl Drop for JobCtlGuard {
    fn drop(&mut self) {
        self.ctl.active.store(false, Ordering::Release);
        self.app.state::<UploadControl>().clear();
    }
}

/// Emit a preparing-phase status to the **frontend only** (file `current` of
/// `total`, with a `message` like "Hashing file 2/10 · 40%"). Used for the
/// throttled intra-file hashing percent — too frequent for the notification.
fn emit_prepare(
    app: &AppHandle,
    job_id: &str,
    current: u64,
    total: u64,
    file: &str,
    message: &str,
) {
    emit(
        app,
        ProgressEvent {
            job_id: job_id.to_string(),
            phase: "scanning".into(),
            current,
            total,
            file: Some(file.to_string()),
            message: Some(message.to_string()),
            ..Default::default()
        },
    );
}

/// Emit a preparing-phase status to **both** the frontend and the OS /
/// foreground notification. Used for per-file stage transitions ("Reading file
/// 2/10", "Hashing file 2/10") so the notification visibly advances per file.
#[allow(clippy::too_many_arguments)]
fn prepare_status(
    app: &AppHandle,
    job_id: &str,
    notif_id: i32,
    current: u64,
    total: u64,
    file: &str,
    message: &str,
) {
    emit_prepare(app, job_id, current, total, file, message);
    notify_progress(app, notif_id, "Uploading music", message, -1);
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
    // Android: persist read access to the picked `content://` URIs so the upload
    // can be re-read + resumed after an accidental process kill. No-op elsewhere.
    crate::upload_session::persist_uri_access(&app, items.iter().map(|i| i.path.clone()).collect());
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
        notify_progress(&app, notif_id, "Uploading music", "Scanning folder…", -1);

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
/// the server's disk. Also flips the local job's cancel flag so its chunk loop
/// stops promptly (rather than waiting to notice the server rejection).
#[tauri::command]
pub async fn uploads_cancel(
    id: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<UploadView> {
    let auth = resolve_auth(&state).await?;
    if let Some(ctl) = app.state::<UploadControl>().get(&id) {
        ctl.cancelled.store(true, Ordering::Release);
    }
    auth.cancel_upload(id).await
}

/// Pause an in-flight upload (manual). Flips the local job flag so chunk sends
/// park immediately, reflects the pause in the UI + notification, and tells the
/// server (state → paused). Owner/admin gated server-side.
#[tauri::command]
pub async fn uploads_pause(
    id: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<UploadView> {
    let auth = resolve_auth(&state).await?;
    if let Some(ctl) = app.state::<UploadControl>().get(&id) {
        ctl.manual_paused.store(true, Ordering::Release);
        emit_paused(&app, &ctl.job_id, &ctl.upload_id, "manual");
        notify_progress(
            &app,
            ctl.notif_id,
            "Upload paused",
            "Paused — tap the app to resume.",
            -1,
        );
    }
    auth.pause_upload(id).await
}

/// Resume a paused upload (manual). Clears the local pause flag (parked chunk
/// sends wake within ~250 ms), resets the stall clock so it doesn't immediately
/// re-pause, and tells the server (state → uploading).
#[tauri::command]
pub async fn uploads_resume(
    id: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<UploadView> {
    let auth = resolve_auth(&state).await?;
    if let Some(ctl) = app.state::<UploadControl>().get(&id) {
        ctl.manual_paused.store(false, Ordering::Release);
        ctl.auto_paused.store(false, Ordering::Release);
        ctl.touch_success(); // restart the stall timer from "now"
        emit_resumed(&app, &ctl.job_id, &ctl.upload_id);
    }
    auth.resume_upload(id).await
}

/// Resume an upload left in flight by a previous app session, if any. Call once
/// on startup (after the session is restored). Returns `true` if a resume was
/// started. No-ops when there's no manifest or an upload is already active, so
/// it's safe to call more than once. The actual work (file re-verification +
/// resuming the remaining chunks) runs as a background job, exactly like a fresh
/// upload — progress + completion arrive over the same `upload-progress` /
/// `upload-complete` events.
#[tauri::command]
pub async fn uploads_resume_pending(
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<bool> {
    let auth = resolve_auth(&state).await?;
    // Atomically claim the resume slot: skip if an upload is already running, or
    // a resume is already in its verify window (`swap` returns the prior value).
    {
        let control = app.state::<UploadControl>();
        if control.is_active() || control.resuming.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }
    }
    let manifest = read_manifest(&app);
    // Clean up staging copies orphaned by a crash — keep the one we're about to
    // resume (if any). Safe here: no upload is active (checked above).
    sweep_staging(
        &app,
        manifest.as_ref().and_then(|m| m.staging_key.as_deref()),
    );
    let Some(manifest) = manifest else {
        // Nothing to resume — release the flag we just claimed.
        app.state::<UploadControl>()
            .resuming
            .store(false, Ordering::Release);
        return Ok(false);
    };
    tracing::info!(upload_id = %manifest.upload_id, files = manifest.files.len(), "resuming upload from a previous session");
    tauri::async_runtime::spawn(async move {
        run_resume_job(app, auth, manifest).await;
    });
    Ok(true)
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
    fn upload_control_matches_active_job_by_id() {
        let control = UploadControl::default();
        let ctl = Arc::new(JobCtl::new("up-1".into(), "job-1".into(), 7));
        control.set(ctl.clone());
        assert!(
            control.get("up-1").is_some(),
            "matching id resolves the job"
        );
        assert!(
            control.get("other").is_none(),
            "a different id does not match"
        );
        control.clear();
        assert!(
            control.get("up-1").is_none(),
            "cleared slot resolves nothing"
        );
    }

    #[test]
    fn resume_manifest_round_trips() {
        let m = ResumeManifest {
            upload_id: "up-1".into(),
            job_id: "job-1".into(),
            staging_key: Some("stage-abc".into()),
            files: vec![ResumeFile {
                path: "/data/data/dev.niruhsa.octave/files/upload_staging/stage-abc/0.flac".into(),
                name_hint: "song.flac".into(),
                hash: "a".repeat(64),
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: ResumeManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.upload_id, "up-1");
        assert_eq!(back.staging_key.as_deref(), Some("stage-abc"));
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.files[0].hash, "a".repeat(64));
        // A pre-staging manifest (no `staging_key`) still deserializes (serde default).
        let old: ResumeManifest =
            serde_json::from_str(r#"{"upload_id":"u","job_id":"j","files":[]}"#).unwrap();
        assert_eq!(old.staging_key, None);
    }

    #[test]
    fn jobctl_flags_and_stall_reset() {
        let ctl = JobCtl::new("u".into(), "j".into(), 1);
        assert!(!ctl.manual_paused.load(Ordering::Acquire));
        ctl.manual_paused.store(true, Ordering::Release);
        assert!(ctl.manual_paused.load(Ordering::Acquire));
        // touch_success keeps the stall window near zero (just stamped "now").
        ctl.touch_success();
        assert!(ctl.stall_ms() < 1_000);
    }

    #[test]
    fn prepare_file_builds_contiguous_chunk_map() {
        let data = vec![7u8; (CHUNK_SIZE + 100) as usize];
        // The single-pass hash reports progress per chunk; assert it lands on the
        // exact byte total at the end (and is monotonic).
        let mut ticks: Vec<(u64, u64)> = Vec::new();
        let f = prepare_file("x.flac".into(), &data, |done, total| {
            ticks.push((done, total))
        });
        assert_eq!(ticks.len(), 2);
        assert_eq!(
            ticks.last().copied(),
            Some((CHUNK_SIZE + 100, CHUNK_SIZE + 100))
        );
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
