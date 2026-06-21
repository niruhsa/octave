//! Upload commands (Phase 8 + background jobs).
//!
//! Uploads run as **background jobs**: `upload_files` / `upload_folder` resolve
//! auth, spawn a task on the async runtime, and return a `jobId` immediately so
//! the UI never blocks. The job:
//!   * emits `upload-progress` events (`{ jobId, phase, current, total, … }`),
//!   * drives a single OS notification that updates per file and is **replaced**
//!     by a completion notification (same notification id) at the end,
//!   * emits a final `upload-complete` event with the tally.
//!
//! Source resolution differs per platform but the job runner is uniform — it
//! just reads filesystem paths:
//!   * **Desktop:** the dialog returns real paths; `upload_folder` walks a dir.
//!   * **Android:** the dialog returns SAF `content://` URIs which the frontend
//!     stages into the app cache (via the fs plugin) and passes here with
//!     `cleanup = true`; the job deletes each temp file after uploading. Since
//!     content-URI filenames are unreliable, [`determine_filename`] sniffs the
//!     real format from the file's magic bytes when the name lacks a known
//!     extension.
//!
//! Manager+ is enforced server-side on every file.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use crate::auth::AuthManager;
use crate::error::{AppError, AppResult};

/// Audio extensions recognised by the server (matches `server/src/services/tag.rs::AUDIO_EXTS`).
const AUDIO_EXTS: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv", "aac", "mp4",
];

/// Sidecar image filenames treated as album art next to a loose audio file.
const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "albumart", "artwork"];
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Monotonic job id source (also seeds the per-job notification id).
static JOB_SEQ: AtomicU64 = AtomicU64::new(1);

// ── Input / output types ─────────────────────────────────────────────────────

/// One source to upload. `name` is an optional display-name hint (used when the
/// path itself has no usable name, e.g. a staged Android temp file); `cleanup`
/// deletes the path after upload (temp files staged from a `content://` URI).
#[derive(Debug, Clone, Deserialize)]
pub struct UploadItem {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cleanup: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressEvent {
    job_id: String,
    phase: String, // "scanning" | "uploading" | "done"
    current: u64,
    total: u64,
    file: Option<String>,
    ok: Option<bool>,
    message: Option<String>,
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

/// Sniff a file format from its leading magic bytes. Used as a fallback when a
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
    if b.len() >= 3 && &b[0..3] == b"ID3" {
        return Some("mp3");
    }
    if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 {
        return Some("mp3"); // MPEG audio frame sync
    }
    None
}

/// Resolve the filename to send to the server: prefer an explicit hint or the
/// path's own name when it already has a recognised extension; otherwise sniff
/// the format from the bytes and synthesise `<stem>.<ext>`.
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
    let stem = candidate
        .as_deref()
        .map(|c| {
            Path::new(c)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(c)
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "upload".to_string());
    format!("{stem}.{ext}")
}

/// Look for a cover image next to a loose audio file. Returns `(name, bytes)`.
async fn sidecar_cover(source: &Path) -> Option<(String, Vec<u8>)> {
    let dir = source.parent()?;
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let stem = path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        let (Some(stem), Some(ext)) = (stem, ext) else { continue };
        if COVER_STEMS.contains(&stem.as_str()) && COVER_EXTS.contains(&ext.as_str()) {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                if !bytes.is_empty() {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("cover.jpg")
                        .to_string();
                    return Some((name, bytes));
                }
            }
        }
    }
    None
}

// ── Notifications ────────────────────────────────────────────────────────────

/// Show/replace the job's notification (best-effort — a denied permission must
/// never fail the upload). The fixed `id` per job makes each call replace the
/// previous one, so progress collapses into the completion notice.
fn notify(app: &AppHandle, id: i32, title: &str, body: &str) {
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .id(id)
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
    let total = items.len() as u64;
    let mut succeeded: u64 = 0;
    let mut failed: u64 = 0;
    let mut skipped: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let path = std::path::PathBuf::from(&item.path);
        let display = item
            .name
            .clone()
            .or_else(|| path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "file".to_string());

        emit(&app, ProgressEvent {
            job_id: job_id.clone(),
            phase: "uploading".into(),
            current: idx as u64,
            total,
            file: Some(display.clone()),
            ok: None,
            message: None,
        });
        notify(&app, notif_id, "Uploading music", &format!("{}/{} · {display}", idx + 1, total));

        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                errors.push(format!("{display}: read error: {e}"));
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(display.clone()),
                    ok: Some(false),
                    message: Some(format!("read error: {e}")),
                });
                cleanup(item).await;
                continue;
            }
        };

        if data.is_empty() {
            skipped += 1;
            cleanup(item).await;
            continue;
        }

        let filename = determine_filename(item.name.as_deref(), &path, &data);

        // Sidecar covers only make sense for loose files still in their real
        // directory (desktop). Staged temp files (cleanup) live alone.
        let cover = if item.cleanup || is_archive_name(&filename) {
            None
        } else {
            sidecar_cover(&path).await
        };

        match auth.upload_file(&filename, data, cover).await {
            Ok(_) => {
                succeeded += 1;
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(display.clone()),
                    ok: Some(true),
                    message: None,
                });
            }
            Err(e) => {
                failed += 1;
                let msg = e.to_string();
                errors.push(format!("{display}: {msg}"));
                emit(&app, ProgressEvent {
                    job_id: job_id.clone(),
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(display.clone()),
                    ok: Some(false),
                    message: Some(msg),
                });
            }
        }

        cleanup(item).await;
    }

    emit(&app, ProgressEvent {
        job_id: job_id.clone(),
        phase: "done".into(),
        current: total,
        total,
        file: None,
        ok: None,
        message: None,
    });

    let _ = app.emit("upload-complete", CompleteEvent {
        job_id: job_id.clone(),
        total,
        succeeded,
        failed,
        skipped,
        errors: errors.clone(),
    });

    // Replace the progress notification with a completion summary.
    let title = if failed > 0 { "Upload finished with errors" } else { "Upload complete" };
    let mut body = format!("{succeeded} uploaded");
    if failed > 0 {
        body.push_str(&format!(" · {failed} failed"));
    }
    if skipped > 0 {
        body.push_str(&format!(" · {skipped} skipped"));
    }
    notify(&app, notif_id, title, &body);
}

async fn cleanup(item: &UploadItem) {
    if item.cleanup {
        let _ = tokio::fs::remove_file(&item.path).await;
    }
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

/// Upload a set of already-resolved files in the background. Returns the job id;
/// progress arrives via `upload-progress` / `upload-complete` events + an OS
/// notification. Used by desktop multi-file selection and by Android after the
/// frontend stages `content://` URIs into the app cache.
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
        emit(&app, ProgressEvent {
            job_id: jid.clone(),
            phase: "scanning".into(),
            current: 0,
            total: 0,
            file: None,
            ok: None,
            message: None,
        });
        notify(&app, notif_id, "Uploading music", "Scanning folder…");

        let items: Vec<UploadItem> = walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| is_uploadable(e.path()))
            .map(|e| UploadItem {
                path: e.path().to_string_lossy().into_owned(),
                name: None,
                cleanup: false,
            })
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
    fn filename_prefers_known_ext_then_sniffs() {
        // Explicit name with a known extension is kept.
        assert_eq!(determine_filename(Some("Song.flac"), Path::new("/tmp/x"), b""), "Song.flac");
        // Nameless content URI → sniff appends the right extension.
        assert_eq!(
            determine_filename(Some("document-12345"), Path::new("/tmp/x"), b"fLaC\0\0\0\0"),
            "document-12345.flac"
        );
        // No name + no ext + unknown bytes → upload.bin.
        assert_eq!(determine_filename(None, Path::new("/tmp/blob"), b"????"), "blob.bin");
    }
}
