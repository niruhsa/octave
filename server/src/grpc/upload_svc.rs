//! gRPC UploadService implementation.
//!
//! Client-streaming: the first message must be an `UploadInfo` (filename),
//! followed by `chunk` messages. The server stages the bytes to a temp file
//! under the ingest `.tmp` area, then runs the same organise+index pipeline
//! as the REST `POST /upload` endpoint (single file or archive). Manager+.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{PermissionLevel, UploadFileState, UploadState};
use crate::error::AppError;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::{AuthInterceptor, extract_credential};
use crate::grpc::proto::upload as pb;
use crate::shutdown::ShutdownRx;
use crate::services::archive::ArchiveKind;
use crate::services::{
    ChunkAck as SvcChunkAck, ChunkInit as SvcChunkInit, FileInit as SvcFileInit, IngestService,
    UploadEvent as SvcUploadEvent, UploadFileView as SvcUploadFileView, UploadHub,
    UploadSummary as SvcUploadSummary, UploadView as SvcUploadView, UploadsService, can_see, tag,
};

/// Hard cap on a streamed upload: 5 GiB (matches the REST `DefaultBodyLimit`).
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct UploadServer {
    pub ingest: Option<IngestService>,
    /// DB-backed upload sessions (Uploads v2). None when no staging dir.
    pub uploads: Option<UploadsService>,
    /// Live-progress broadcast hub, shared with the REST WebSocket.
    pub hub: UploadHub,
    pub interceptor: AuthInterceptor,
    /// Server shutdown flag — ends the otherwise-endless `StreamUploads`
    /// response so a connected client can't block graceful shutdown.
    pub shutdown: ShutdownRx,
}

impl UploadServer {
    pub fn into_service(self) -> pb::upload_service_server::UploadServiceServer<Self> {
        pb::upload_service_server::UploadServiceServer::new(self)
    }

    fn uploads_svc(&self) -> Result<&UploadsService, Status> {
        self.uploads
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("uploads service not configured"))
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
        caller.require(PermissionLevel::Manager).map_err(map_err)?;

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
                return Err(Status::invalid_argument("first message must be UploadInfo"));
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

    // ---- Uploads v2: delegate to the shared UploadsService ----

    async fn init_upload(
        &self,
        req: Request<pb::InitUploadRequest>,
    ) -> Result<Response<pb::UploadView>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let files = req
            .into_inner()
            .files
            .into_iter()
            .map(file_init_from_pb)
            .collect();
        let view = svc.init(&caller, files).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn put_chunk(
        &self,
        req: Request<pb::PutChunkRequest>,
    ) -> Result<Response<pb::ChunkAck>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let r = req.into_inner();
        let id = parse_uuid(&r.upload_id)?;
        let ack = svc
            .put_chunk(
                &caller,
                id,
                r.file_index as i32,
                r.chunk_index as i32,
                &r.data,
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(ack_to_pb(ack)))
    }

    async fn get_upload(
        &self,
        req: Request<pb::GetUploadRequest>,
    ) -> Result<Response<pb::UploadView>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let id = parse_uuid(&req.into_inner().upload_id)?;
        let view = svc.get(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn list_uploads(
        &self,
        req: Request<pb::ListUploadsRequest>,
    ) -> Result<Response<pb::ListUploadsResponse>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let r = req.into_inner();
        let user = if r.user_id.is_empty() {
            None
        } else {
            Some(parse_uuid(&r.user_id)?)
        };
        let state =
            if r.state.is_empty() {
                None
            } else {
                Some(UploadState::parse(&r.state).ok_or_else(|| {
                    Status::invalid_argument(format!("invalid state: {}", r.state))
                })?)
            };
        let limit = if r.limit <= 0 { 50 } else { r.limit };
        let rows = svc
            .list(&caller, user, state, limit, r.offset)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListUploadsResponse {
            uploads: rows.into_iter().map(summary_to_pb).collect(),
        }))
    }

    async fn cancel_upload(
        &self,
        req: Request<pb::CancelUploadRequest>,
    ) -> Result<Response<pb::UploadView>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let id = parse_uuid(&req.into_inner().upload_id)?;
        let view = svc.cancel(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn pause_upload(
        &self,
        req: Request<pb::PauseUploadRequest>,
    ) -> Result<Response<pb::UploadView>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let id = parse_uuid(&req.into_inner().upload_id)?;
        let view = svc.pause(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    async fn resume_upload(
        &self,
        req: Request<pb::ResumeUploadRequest>,
    ) -> Result<Response<pb::UploadView>, Status> {
        let caller = self.interceptor.resolve(&req).await?;
        let svc = self.uploads_svc()?;
        let id = parse_uuid(&req.into_inner().upload_id)?;
        let view = svc.resume(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(view_to_pb(view)))
    }

    type StreamUploadsStream = Pin<Box<dyn Stream<Item = Result<pb::UploadEvent, Status>> + Send>>;

    async fn stream_uploads(
        &self,
        req: Request<pb::StreamUploadsRequest>,
    ) -> Result<Response<Self::StreamUploadsStream>, Status> {
        let identity = self.interceptor.resolve(&req).await?;
        let mut rx = self.hub.subscribe();
        let mut shutdown = self.shutdown.clone();

        // Forward events this listener may see (admin → all; user → own) into a
        // per-client channel. The task — and therefore the response stream —
        // ends when the client hangs up, the hub closes, or the server starts
        // shutting down. That last exit is the important one: without it this
        // stream would stay open forever and wedge the graceful drain.
        let (tx, out) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    event = rx.recv() => match event {
                        Ok(ev) => {
                            if can_see(&identity, ev.owner_id)
                                && tx.send(Ok(event_to_pb(ev))).await.is_err()
                            {
                                break; // client dropped the stream
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(out))))
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

// ---------------------------------------------------------------------------
// proto <-> service conversions
// ---------------------------------------------------------------------------

fn parse_uuid(s: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|e| Status::invalid_argument(format!("invalid upload id: {e}")))
}

fn state_str(s: UploadState) -> String {
    match s {
        UploadState::Initialized => "initialized",
        UploadState::Uploading => "uploading",
        UploadState::Paused => "paused",
        UploadState::Completed => "completed",
        UploadState::Cancelled => "cancelled",
    }
    .to_string()
}

fn file_state_str(s: UploadFileState) -> String {
    match s {
        UploadFileState::Pending => "pending",
        UploadFileState::Uploading => "uploading",
        UploadFileState::Complete => "complete",
        UploadFileState::Failed => "failed",
    }
    .to_string()
}

fn file_init_from_pb(f: pb::FileInit) -> SvcFileInit {
    SvcFileInit {
        filename: f.filename,
        hash: f.hash,
        total_size: f.total_size as i64,
        chunk_size: f.chunk_size as i64,
        total_chunks: f.total_chunks as i32,
        chunks: f
            .chunks
            .into_iter()
            .map(|c| SvcChunkInit {
                index: c.index as i32,
                start: c.start as i64,
                end: c.end as i64,
                hash: c.hash,
            })
            .collect(),
    }
}

fn view_to_pb(v: SvcUploadView) -> pb::UploadView {
    pb::UploadView {
        id: v.id,
        user_id: v.user_id.unwrap_or_default(),
        state: state_str(v.state),
        total_files: v.total_files as u32,
        total_bytes: v.total_bytes as u64,
        bytes_received: v.bytes_received as u64,
        created_at: v.created_at,
        updated_at: v.updated_at,
        error: v.error.unwrap_or_default(),
        report_json: v.report.map(|r| r.to_string()).unwrap_or_default(),
        files: v.files.into_iter().map(file_view_to_pb).collect(),
    }
}

fn file_view_to_pb(f: SvcUploadFileView) -> pb::UploadFileView {
    pb::UploadFileView {
        file_index: f.file_index as u32,
        filename: f.filename,
        file_hash: f.file_hash,
        total_size: f.total_size as u64,
        chunk_size: f.chunk_size as u64,
        total_chunks: f.total_chunks as u32,
        received_chunks: f.received_chunks as u32,
        state: file_state_str(f.state),
        error: f.error.unwrap_or_default(),
        chunks: f
            .chunks
            .into_iter()
            .map(|c| pb::ChunkView {
                index: c.index as u32,
                start: c.start as u64,
                end: c.end as u64,
                hash: c.hash,
                received: c.received,
            })
            .collect(),
    }
}

fn summary_to_pb(s: SvcUploadSummary) -> pb::UploadSummary {
    pb::UploadSummary {
        id: s.id,
        user_id: s.user_id.unwrap_or_default(),
        state: state_str(s.state),
        total_files: s.total_files as u32,
        total_bytes: s.total_bytes as u64,
        created_at: s.created_at,
        updated_at: s.updated_at,
        error: s.error.unwrap_or_default(),
    }
}

fn ack_to_pb(a: SvcChunkAck) -> pb::ChunkAck {
    pb::ChunkAck {
        file_index: a.file_index as u32,
        chunk_index: a.chunk_index as u32,
        received_chunks: a.received_chunks as u32,
        total_chunks: a.total_chunks as u32,
        file_complete: a.file_complete,
        upload_complete: a.upload_complete,
        state: state_str(a.state),
    }
}

fn event_to_pb(ev: SvcUploadEvent) -> pb::UploadEvent {
    pb::UploadEvent {
        kind: ev.kind,
        upload_id: ev.upload_id,
        owner_id: ev.owner_id.map(|x| x.to_string()).unwrap_or_default(),
        state: state_str(ev.state),
        file_index: ev.file_index.unwrap_or(-1),
        total_files: ev.total_files as u32,
        bytes_received: ev.bytes_received as u64,
        total_bytes: ev.total_bytes as u64,
        chunks_received: ev.chunks_received as u32,
        total_chunks: ev.total_chunks as u32,
        bytes_per_sec: ev.bytes_per_sec.unwrap_or(0.0),
        report_json: ev.report.map(|r| r.to_string()).unwrap_or_default(),
    }
}
