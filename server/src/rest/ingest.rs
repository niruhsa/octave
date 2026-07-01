//! REST ingest routes — feature parity with `IngestService` operations.

use std::path::{Path, PathBuf};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Extension, Multipart, Request, State},
    http::StatusCode,
    routing::post,
};
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::error::AppError;
use crate::rest::ApiError;
use crate::services::archive::ArchiveKind;
use crate::services::tag;

/// Max upload size: 500 MiB.  Applied via [`DefaultBodyLimit`].
const MAX_UPLOAD_BYTES: usize = 500 * 1024 * 1024;

pub fn router() -> Router<crate::rest::RestState> {
    Router::new()
        .route("/upload", post(upload))
        .route("/ingest/scan", post(ingest_scan))
        .route_layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
}

// ---------------------------------------------------------------------------
// Upload (multipart)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct UploadResponse {
    pub track_id: String,
    pub path: String,
}

/// Response for an archive upload (zip/tarball) — multiple tracks ingested.
#[derive(Serialize, Deserialize)]
pub struct ArchiveUploadResponse {
    pub kind: String,
    pub ingested: u64,
    pub already_indexed: u64,
    pub non_audio_skipped: u64,
    pub errors: u64,
    pub track_ids: Vec<String>,
}

/// Either a single-file or an archive upload result.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum UploadResult {
    Single(UploadResponse),
    Archive(ArchiveUploadResponse),
}

async fn upload(
    State(state): State<crate::rest::RestState>,
    Extension(caller): Extension<Identity>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResult>), ApiError> {
    caller.require(PermissionLevel::Manager).map_err(ApiError::from)?;

    let ingest = state
        .ingest
        .as_ref()
        .ok_or_else(|| AppError::Config("ingest service not configured".into()))
        .map_err(ApiError::from)?;

    // The primary audio/archive file plus an optional cover image carried in
    // a separate multipart field (so single-file uploads can ship art that
    // would otherwise only exist as a sidecar in a folder/archive upload).
    let mut source: Option<(String, Vec<u8>)> = None;
    let mut cover: Option<(String, Vec<u8>)> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::InvalidArgument(format!("multipart field: {e}")))
        .map_err(ApiError::from)?
    {
        let field_name = field.name().map(|s| s.to_string());
        let content_type = field.content_type().map(|s| s.to_string());
        let filename = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let name = field_name.clone().unwrap_or_else(|| "upload".to_string());
                let ct = content_type
                    .as_deref()
                    .and_then(mime_to_ext)
                    .unwrap_or("bin");
                format!("{name}.{ct}")
            });

        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("read upload: {e}")))
            .map_err(ApiError::from)?;

        // Classify as a cover image when the field is named `cover` (or
        // similar), the content-type is an image, or the filename has an
        // image extension. Everything else is the primary source file.
        if is_cover_field(field_name.as_deref(), content_type.as_deref(), &filename) {
            if cover.is_none() {
                cover = Some((filename, data.to_vec()));
            }
        } else if source.is_none() {
            source = Some((filename, data.to_vec()));
        }
    }

    let (filename, data) = source.ok_or_else(|| {
        ApiError::from(AppError::InvalidArgument(
            "upload requires a file field".into(),
        ))
    })?;

    // Stage each upload in its own per-upload subdir under INGEST_PATH/.tmp
    // so an accompanying `cover.<ext>` sidecar is isolated from concurrent
    // uploads (the ingest pipeline's sidecar scan reads the source's parent
    // dir). The leading-dot `.tmp` keeps the folder-watcher from re-ingesting
    // staged files.
    let ingest_root = ingest.ingest_root.as_deref().unwrap_or(Path::new("."));
    let upload_dir = ingest_root.join(".tmp").join(Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
        .map_err(ApiError::from)?;

    // Audio/archive file keeps its real extension so ingest can detect it.
    let ext = Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let source_path = upload_dir.join(format!("source.{ext}"));
    if let Err(e) = tokio::fs::write(&source_path, &data).await {
        let _ = tokio::fs::remove_dir_all(&upload_dir).await;
        return Err(
            AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())).into(),
        );
    }

    // Write the cover sidecar as `cover.<imgext>` next to the source so the
    // ingest pipeline's `local_cover` picks it up before any remote fetch.
    if let Some((cover_name, cover_bytes)) = &cover {
        let cover_ext = Path::new(cover_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("jpg");
        let cover_path = upload_dir.join(format!("cover.{cover_ext}"));
        if let Err(e) = tokio::fs::write(&cover_path, cover_bytes).await {
            // Non-fatal: log and proceed without the cover.
            debug!(error = %e, "upload: cover sidecar write failed");
        }
    }

    // Archive uploads (zip/tarball/disc-image) take the multi-file path;
    // everything else is treated as a single audio file.
    if let Some(kind) = ArchiveKind::detect(Path::new(&filename)) {
        let outcome = ingest.organize_archive(&caller, &source_path, kind).await;
        let _ = tokio::fs::remove_dir_all(&upload_dir).await;
        let res = outcome?;
        debug!(
            ingested = res.ingested,
            errors = res.errors,
            "upload: archive complete"
        );
        return Ok((
            StatusCode::CREATED,
            Json(UploadResult::Archive(ArchiveUploadResponse {
                kind: format!("{kind:?}"),
                ingested: res.ingested,
                already_indexed: res.already_indexed,
                non_audio_skipped: res.non_audio_skipped,
                errors: res.errors,
                track_ids: res.track_ids.iter().map(|id| id.to_string()).collect(),
            })),
        ));
    }

    let result = match ingest.organize_and_index(&caller, &source_path).await {
        Ok(r) => {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            r
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(e.into());
        }
    };

    debug!(track_id = %result.track_id, "upload: complete");

    Ok((
        StatusCode::CREATED,
        Json(UploadResult::Single(UploadResponse {
            track_id: result.track_id.to_string(),
            path: result.dest.to_string_lossy().into_owned(),
        })),
    ))
}

// ---------------------------------------------------------------------------
// Ingest scan
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct IngestScanBody {
    /// Override the default ingest root.
    pub root: Option<String>,
}

#[derive(Serialize)]
pub struct IngestScanResponse {
    pub files_processed: u64,
    pub files_skipped: u64,
    pub errors: u64,
}

async fn ingest_scan(
    State(state): State<crate::rest::RestState>,
    Extension(caller): Extension<Identity>,
    req: Request<Body>,
) -> Result<Json<IngestScanResponse>, ApiError> {
    caller.require(PermissionLevel::Manager).map_err(ApiError::from)?;

    let body: IngestScanBody = crate::rest::parse_json(req).await?;
    let ingest = state
        .ingest
        .as_ref()
        .ok_or_else(|| AppError::Config("ingest service not configured".into()))
        .map_err(ApiError::from)?;

    let root = body
        .root
        .map(PathBuf::from)
        .or_else(|| ingest.ingest_root.clone())
        .ok_or_else(|| {
            AppError::InvalidArgument(
                "no ingest root provided and INGEST_PATH is unset".into(),
            )
        })?;

    if !root.is_dir() {
        return Err(ApiError::from(AppError::InvalidArgument(format!(
            "{} is not a directory",
            root.display()
        ))));
    }

    let mut files_processed: u64 = 0;
    let mut files_skipped: u64 = 0;
    let mut errors: u64 = 0;

    // Collect the distinct parent directories of every audio file, then ingest
    // each folder as a single album (matching the watcher's folder-grouped
    // behaviour) instead of one album per file.
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !tag::is_audio_file(path) {
            continue;
        }
        // Skip staging files from concurrent uploads.
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("uploading"))
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(parent) = path.parent() {
            let parent = parent.to_path_buf();
            if !dirs.contains(&parent) {
                dirs.push(parent);
            }
        }
    }

    for dir in &dirs {
        match ingest.organize_dir(&caller, dir).await {
            Ok(r) => {
                files_processed += r.ingested;
                files_skipped += r.already_indexed;
                errors += r.errors;
            }
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "ingest scan: failed");
                errors += 1;
            }
        }
    }

    Ok(Json(IngestScanResponse {
        files_processed,
        files_skipped,
        errors,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify a multipart field as the cover image rather than the primary
/// source file. Matches when the field is named like a cover, its
/// content-type is `image/*`, or its filename has an image extension.
fn is_cover_field(
    field_name: Option<&str>,
    content_type: Option<&str>,
    filename: &str,
) -> bool {
    const COVER_FIELD_NAMES: &[&str] = &["cover", "artwork", "art", "image", "thumbnail"];
    const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

    if let Some(name) = field_name {
        if COVER_FIELD_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    if let Some(ct) = content_type {
        if ct.to_ascii_lowercase().starts_with("image/") {
            return true;
        }
    }
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn mime_to_ext(mime: &str) -> Option<&str> {
    match mime {
        "audio/mpeg" => Some("mp3"),
        "audio/flac" => Some("flac"),
        "audio/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/mp4" | "audio/m4a" => Some("m4a"),
        "audio/aac" => Some("aac"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/aiff" | "audio/x-aiff" => Some("aiff"),
        "audio/ape" | "audio/x-ape" => Some("ape"),
        "audio/wavpack" => Some("wv"),
        _ => None,
    }
}
