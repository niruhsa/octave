//! gRPC UploadService implementation.
//!
//! Client-streaming: the first message must be an `UploadInfo` (filename),
//! followed by `chunk` messages. The server stages the bytes to a temp file
//! under the ingest `.tmp` area, then runs the same organise+index pipeline
//! as the REST `POST /upload` endpoint (single file or archive). Manager+.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::error::AppError;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::{extract_credential, AuthInterceptor};
use crate::grpc::proto::upload as pb;
use crate::services::archive::ArchiveKind;
use crate::services::{tag, IngestService};

/// Hard cap on a streamed upload: 500 MiB (matches the REST `DefaultBodyLimit`).
const MAX_UPLOAD_BYTES: usize = 500 * 1024 * 1024;

#[derive(Clone)]
pub struct UploadServer {
    pub ingest: Option<IngestService>,
    pub interceptor: AuthInterceptor,
}

impl UploadServer {
    pub fn into_service(self) -> pb::upload_service_server::UploadServiceServer<Self> {
        pb::upload_service_server::UploadServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl pb::upload_service_server::UploadService for UploadServer {
    async fn upload(
        &self,
        req: Request<Streaming<pb::UploadRequest>>,
    ) -> Result<Response<pb::UploadResponse>, Status> {
        // Resolve the caller from metadata *before* taking the stream: a
        // `Streaming<T>` isn't `Sync`, so holding `&Request<Streaming<_>>`
        // across an await would make this future non-Send. Pull the owned
        // credential out first, then consume the request.
        let cred = extract_credential(req.metadata())
            .ok_or_else(|| Status::unauthenticated("missing Authorization metadata"))?;
        let caller: Identity = self.interceptor.resolve_credential(cred).await?;
        caller
            .require(PermissionLevel::Manager)
            .map_err(map_err)?;

        let ingest = self
            .ingest
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("ingest service not configured"))?;

        let mut stream = req.into_inner();

        // 1. First message must be UploadInfo.
        let first = stream
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty upload stream"))?;
        let (filename, cover, cover_filename) = match first.payload {
            Some(pb::upload_request::Payload::Info(info)) => {
                (info.filename, info.cover, info.cover_filename)
            }
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be UploadInfo",
                ));
            }
        };
        if filename.trim().is_empty() {
            return Err(Status::invalid_argument("filename is required"));
        }

        // 2. Stage in a per-upload subdir under <ingest_root>/.tmp/<uuid>/ so
        //    an optional `cover.<ext>` sidecar is isolated from concurrent
        //    uploads (ingest's sidecar scan reads the source's parent dir).
        let ingest_root = ingest.ingest_root.as_deref().unwrap_or(Path::new("."));
        let upload_dir = ingest_root.join(".tmp").join(Uuid::new_v4().to_string());
        tokio::fs::create_dir_all(&upload_dir)
            .await
            .map_err(|e| Status::internal(format!("create tmp dir: {e}")))?;

        let ext = Path::new(&filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let source_path = upload_dir.join(format!("source.{ext}"));

        // 3. Drain chunks to disk (with size cap).
        if let Err(e) = drain_to_file(&mut stream, &source_path).await {
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            return Err(e);
        }

        // 3b. Write the optional cover sidecar as `cover.<imgext>` next to
        //     the source so `local_cover` picks it up before remote fetch.
        if !cover.is_empty() {
            let cover_ext = Path::new(&cover_filename)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg");
            let cover_path = upload_dir.join(format!("cover.{cover_ext}"));
            if let Err(e) = tokio::fs::write(&cover_path, &cover).await {
                tracing::debug!(error = %e, "upload: cover sidecar write failed");
            }
        }

        // 4. Archive vs single-file ingest, then clean up the staged dir.
        let resp = if let Some(kind) = ArchiveKind::detect(Path::new(&filename)) {
            let outcome = ingest.organize_archive(&caller, &source_path, kind).await;
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            let res = outcome.map_err(map_err)?;
            pb::UploadResponse {
                is_archive: true,
                single_track_id: String::new(),
                path: String::new(),
                archive_kind: format!("{kind:?}"),
                ingested: res.ingested as i64,
                already_indexed: res.already_indexed as i64,
                non_audio_skipped: res.non_audio_skipped as i64,
                errors: res.errors as i64,
                track_ids: res.track_ids.iter().map(|id| id.to_string()).collect(),
            }
        } else {
            // Reject obvious non-audio before the pipeline does, for a clearer error.
            if !tag::is_audio_file(Path::new(&filename)) {
                let _ = tokio::fs::remove_dir_all(&upload_dir).await;
                return Err(Status::invalid_argument(format!(
                    "unsupported file type: {filename}"
                )));
            }
            let outcome = ingest.organize_and_index(&caller, &source_path).await;
            let _ = tokio::fs::remove_dir_all(&upload_dir).await;
            let res = outcome.map_err(map_err)?;
            pb::UploadResponse {
                is_archive: false,
                single_track_id: res.track_id.to_string(),
                path: res.dest.to_string_lossy().into_owned(),
                archive_kind: String::new(),
                ingested: 0,
                already_indexed: 0,
                non_audio_skipped: 0,
                errors: 0,
                track_ids: vec![],
            }
        };

        Ok(Response::new(resp))
    }
}

/// Stream the remaining `chunk` messages to `path`, enforcing the size cap.
async fn drain_to_file(
    stream: &mut Streaming<pb::UploadRequest>,
    path: &PathBuf,
) -> Result<(), Status> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| Status::internal(format!("create staged file: {e}")))?;
    let mut total: usize = 0;

    while let Some(msg) = stream.message().await? {
        match msg.payload {
            Some(pb::upload_request::Payload::Chunk(bytes)) => {
                total = total.saturating_add(bytes.len());
                if total > MAX_UPLOAD_BYTES {
                    return Err(Status::invalid_argument(format!(
                        "upload exceeds {MAX_UPLOAD_BYTES} byte limit"
                    )));
                }
                file.write_all(&bytes)
                    .await
                    .map_err(|e| Status::internal(format!("write chunk: {e}")))?;
            }
            Some(pb::upload_request::Payload::Info(_)) => {
                return Err(Status::invalid_argument(
                    "unexpected UploadInfo after first message",
                ));
            }
            None => continue,
        }
    }

    file.flush()
        .await
        .map_err(|e| Status::internal(format!("flush staged file: {e}")))?;
    if total == 0 {
        return Err(Status::invalid_argument("upload contained no data"));
    }
    Ok(())
}

// Keep AppError import meaningful for future use without dead-code warnings.
#[allow(dead_code)]
fn _force(_: AppError) {}
