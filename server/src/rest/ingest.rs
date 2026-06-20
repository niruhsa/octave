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

#[derive(Serialize)]
pub struct UploadResponse {
    pub track_id: String,
    pub path: String,
}

/// Response for an archive upload (zip/tarball) — multiple tracks ingested.
#[derive(Serialize)]
pub struct ArchiveUploadResponse {
    pub kind: String,
    pub ingested: u64,
    pub already_indexed: u64,
    pub non_audio_skipped: u64,
    pub errors: u64,
    pub track_ids: Vec<String>,
}

/// Either a single-file or an archive upload result.
#[derive(Serialize)]
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

    let mut source: Option<(String, Vec<u8>)> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::InvalidArgument(format!("multipart field: {e}")))
        .map_err(ApiError::from)?
    {
        let filename = field
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let name = field.name().unwrap_or("upload").to_string();
                let ct = field
                    .content_type()
                    .and_then(|m| mime_to_ext(m))
                    .unwrap_or("bin");
                format!("{name}.{ct}")
            });

        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("read upload: {e}")))
            .map_err(ApiError::from)?;

        if source.is_none() {
            source = Some((filename, data.to_vec()));
        }
    }

    let (filename, data) = source.ok_or_else(|| {
        ApiError::from(AppError::InvalidArgument(
            "upload requires a file field".into(),
        ))
    })?;

    // Write to a temp file inside INGEST_PATH/.tmp with a `.uploading`
    // extension so the background watcher ignores it.
    let ingest_root = ingest.ingest_root.as_deref().unwrap_or(Path::new("."));
    let tmp_dir = ingest_root.join(".tmp");
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
        .map_err(ApiError::from)?;

    let tmp_path = tmp_dir.join(format!("{}.uploading", Uuid::new_v4()));
    tokio::fs::write(&tmp_path, &data)
        .await
        .map_err(|e| AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))
        .map_err(ApiError::from)?;

    // Rename to real extension so ingest sees it as audio.
    let ext = Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let source_path = tmp_path.with_extension(ext);
    if let Err(e) = tokio::fs::rename(&tmp_path, &source_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(
            AppError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())).into(),
        );
    }

    // Archive uploads (zip/tarball/disc-image) take the multi-file path;
    // everything else is treated as a single audio file.
    if let Some(kind) = ArchiveKind::detect(Path::new(&filename)) {
        let outcome = ingest.organize_archive(&caller, &source_path, kind).await;
        let _ = tokio::fs::remove_file(&source_path).await;
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
            let _ = tokio::fs::remove_file(&source_path).await;
            r
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&source_path).await;
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
        match ingest.organize_and_index(&caller, path).await {
            Ok(_) => files_processed += 1,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already indexed") {
                    files_skipped += 1;
                } else {
                    tracing::warn!(path = %path.display(), error = %e, "ingest scan: failed");
                    errors += 1;
                }
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
