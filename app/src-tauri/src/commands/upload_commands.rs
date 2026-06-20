//! Upload commands (Phase 8).
//!
//! Expose `upload_file` to the frontend. The command reads the file from a
//! path returned by `tauri-plugin-dialog`, then pushes it to the server via
//! gRPC (client-streaming) or REST (multipart) fallback. Manager+ gated
//! server-side.

use std::path::Path;

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::transport::UploadResult;

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

    let result = auth.upload_file(&filename, data).await?;
    Ok(result.into())
}