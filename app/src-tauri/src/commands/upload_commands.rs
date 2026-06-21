//! Upload commands (Phase 8).
//!
//! Expose `upload_file` and `upload_folder` to the frontend. Commands read
//! local paths returned by `tauri-plugin-dialog`, then push files to the
//! server via gRPC (client-streaming) or REST (multipart) fallback.
//! Manager+ gated server-side.
//!
//! `upload_folder` walks a directory recursively, collects every uploadable
//! file (audio + archive), then uploads each one in sequence.  Progress is
//! reported via `upload-progress` Tauri events.

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::error::{AppError, AppResult};
use crate::transport::UploadResult;

/// Audio extensions recognised by the server (matches `server/src/services/tag.rs::AUDIO_EXTS`).
const AUDIO_EXTS: &[&str] = &[
    "flac", "mp3", "ogg", "opus", "m4a", "wav", "aiff", "ape", "wv",
    "aac", "mp4",
];

/// Sidecar image filenames (case-insensitive stem) treated as album cover
/// art when found next to an uploaded audio file. Mirrors the server's
/// `ingest::COVER_STEMS`.
const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "albumart", "artwork"];
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Look for a cover image sitting next to `source` (a single audio file).
/// Returns `(filename, bytes)` for the first match, else `None`.
///
/// Archives carry their own embedded sidecar so this only applies to loose
/// single-file uploads where the cover lives beside the track on disk.
async fn sidecar_cover(source: &Path) -> Option<(String, Vec<u8>)> {
    let dir = source.parent()?;
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let (Some(stem), Some(ext)) = (stem, ext) else {
            continue;
        };
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

/// True when `filename` looks like a recognised archive/disc-image (the
/// server detects these by full filename suffix, e.g. `.tar.gz`).
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

    // Single-extension audio
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

// ── JSON response types ────────────────────────────────────────────────────

/// Result summary safe to serialize over the bridge.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "variant", content = "data")]
pub enum UploadResultJson {
    #[serde(rename = "single")]
    Single {
        track_id: String,
        path: String,
    },
    #[serde(rename = "archive")]
    Archive {
        kind: String,
        ingested: u64,
        already_indexed: u64,
        non_audio_skipped: u64,
        errors: u64,
        track_ids: Vec<String>,
    },
}

impl From<UploadResult> for UploadResultJson {
    fn from(r: UploadResult) -> Self {
        match r {
            UploadResult::Single(s) => UploadResultJson::Single {
                track_id: s.track_id,
                path: s.path,
            },
            UploadResult::Archive(a) => UploadResultJson::Archive {
                kind: a.kind,
                ingested: a.ingested,
                already_indexed: a.already_indexed,
                non_audio_skipped: a.non_audio_skipped,
                errors: a.errors,
                track_ids: a.track_ids,
            },
        }
    }
}

/// Summary for a folder-upload batch.
#[derive(Debug, Clone, Serialize)]
pub struct FolderUploadResultJson {
    pub total: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
}

/// One progress event emitted during folder upload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadProgress {
    phase: String,        // "scanning" | "uploading" | "done"
    current: u64,
    total: u64,
    file: Option<String>, // filename being uploaded
    ok: Option<bool>,     // result of the last file (None = in-progress / scan)
    message: Option<String>,
}

// ── Commands ───────────────────────────────────────────────────────────────

/// Upload a single file (audio track or archive) to the server.
///
/// `path` is an absolute file path — typically from a native file-picker
/// dialog (see `tauri-plugin-dialog`). The file is read locally then pushed
/// to the server. Manager+ enforced server-side.
#[tauri::command]
pub async fn upload_file(
    path: String,
    state: tauri::State<'_, crate::AppStateHandle>,
) -> AppResult<UploadResultJson> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };

    let source = Path::new(&path);
    if !source.is_file() {
        return Err(AppError::Internal(format!(
            "not a file: {}",
            source.display()
        )));
    }

    let filename = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload.bin")
        .to_string();

    let data = tokio::fs::read(source)
        .await
        .map_err(|e| AppError::Internal(format!("read file: {e}")))?;

    if data.is_empty() {
        return Err(AppError::Internal("file is empty".into()));
    }

    // For a single audio file, look for a sidecar cover image next to it so
    // the server can attach album art (archives carry their own sidecar).
    let cover = if is_archive_name(&filename) {
        None
    } else {
        sidecar_cover(source).await
    };

    let result = auth.upload_file(&filename, data, cover).await?;
    Ok(result.into())
}

/// Upload every audio and archive file in a directory tree.
///
/// Walks `dir_path` recursively, collects uploadable files (audio +
/// recognised archives), then pushes each one to the server in sequence.
/// Emits `upload-progress` events so the UI can show per-file progress.
/// Manager+ gated server-side.
#[tauri::command]
pub async fn upload_folder(
    dir_path: String,
    state: tauri::State<'_, crate::AppStateHandle>,
    app: AppHandle,
) -> AppResult<FolderUploadResultJson> {
    let auth = {
        let guard = state.auth.read().await;
        guard
            .clone()
            .ok_or_else(|| AppError::AuthNotConfigured("log in first".into()))?
    };

    let root = Path::new(&dir_path);
    if !root.is_dir() {
        return Err(AppError::Internal(format!(
            "not a directory: {}",
            root.display()
        )));
    }

    // ── Scan pass ──
    let _ = app.emit("upload-progress", UploadProgress {
        phase: "scanning".into(),
        current: 0,
        total: 0,
        file: None,
        ok: None,
        message: None,
    });

    let files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| is_uploadable(e.path()))
        .map(|e| e.path().to_path_buf())
        .collect();

    let total = files.len() as u64;
    if total == 0 {
        return Ok(FolderUploadResultJson {
            total: 0,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            errors: vec![],
        });
    }

    // ── Upload pass ──
    let mut succeeded: u64 = 0;
    let mut failed: u64 = 0;
    let mut skipped: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    for (idx, path) in files.iter().enumerate() {
        let fname = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        // Emit "uploading this file" event.
        let _ = app.emit("upload-progress", UploadProgress {
            phase: "uploading".into(),
            current: idx as u64,
            total,
            file: Some(fname.clone()),
            ok: None,
            message: None,
        });

        let data = match tokio::fs::read(path).await {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                errors.push(format!("{fname}: read error: {e}"));
                let _ = app.emit("upload-progress", UploadProgress {
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(fname),
                    ok: Some(false),
                    message: Some(format!("read error: {e}")),
                });
                continue;
            }
        };

        if data.is_empty() {
            skipped += 1;
            continue;
        }

        // Attach a sidecar cover for loose audio files (not archives).
        let cover = if is_archive_name(&fname) {
            None
        } else {
            sidecar_cover(path).await
        };

        match auth.upload_file(&fname, data, cover).await {
            Ok(_) => {
                succeeded += 1;
                let _ = app.emit("upload-progress", UploadProgress {
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(fname),
                    ok: Some(true),
                    message: None,
                });
            }
            Err(e) => {
                failed += 1;
                let msg = e.to_string();
                errors.push(format!("{fname}: {msg}"));
                let _ = app.emit("upload-progress", UploadProgress {
                    phase: "uploading".into(),
                    current: idx as u64 + 1,
                    total,
                    file: Some(fname),
                    ok: Some(false),
                    message: Some(msg),
                });
            }
        }
    }

    let _ = app.emit("upload-progress", UploadProgress {
        phase: "done".into(),
        current: total,
        total,
        file: None,
        ok: None,
        message: None,
    });

    Ok(FolderUploadResultJson {
        total,
        succeeded,
        failed,
        skipped,
        errors,
    })
}