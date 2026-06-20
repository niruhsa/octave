//! Streaming service: resolve a track id to an on-disk file, with
//! defense-in-depth against path traversal.
//!
//! The REST layer wraps the result of `resolve` in an HTTP response with
//! `Range` semantics. We keep all of the *security-relevant* logic here
//! (the transport layer should only handle bytes-to-the-wire).
//!
//! Path-resolution policy:
//!   * `Track.file_path` is whatever the library service / scan stored.
//!     It may be absolute or relative.
//!   * If a `library_root` is configured, relative `file_path`s are
//!     joined against it; the canonical resolved path **must** live
//!     under the canonical `library_root` or we refuse the request.
//!   * If no `library_root` is configured, we only accept *absolute*
//!     `file_path`s (and the file must exist). Relative paths in that
//!     mode have no anchor and are rejected.
//!   * Symlinks are followed via `canonicalize`; the canonical check
//!     then catches symlinks pointing outside the root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::fs;
use tracing::warn;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::repo::TrackRepo;
use crate::error::{AppError, Result};

/// A file on disk that's safe to stream to the caller. The REST layer
/// only ever sees fields here — not the raw `Track` or its file_path —
/// so it can't accidentally bypass the safety check.
#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub track_id: Uuid,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// MIME from extension; never trusts arbitrary input.
    pub content_type: &'static str,
}

#[derive(Clone)]
pub struct StreamingService {
    tracks: Arc<dyn TrackRepo>,
    library_root: Option<PathBuf>,
}

impl StreamingService {
    pub fn new(tracks: Arc<dyn TrackRepo>, library_root: Option<PathBuf>) -> Self {
        // Canonicalize once at construction so every per-request check
        // compares against the real root, not a user-supplied alias.
        let library_root = library_root.and_then(|p| match std::fs::canonicalize(&p) {
            Ok(c) => Some(c),
            Err(e) => {
                warn!(root = %p.display(), error = %e, "library_root canonicalize failed; streaming will reject relative paths");
                None
            }
        });
        Self { tracks, library_root }
    }

    /// Any authenticated user may stream (PLAN.md Phase 4 §2). The
    /// `Identity` is taken so future per-user policies (rate limits,
    /// per-track ACLs) plug in here without churning callers.
    pub async fn resolve(&self, _caller: &Identity, track_id: Uuid) -> Result<ResolvedStream> {
        let track = self
            .tracks
            .get(track_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("track {track_id}")))?;

        let resolved = self.resolve_file_path(&track.file_path)?;
        let meta = fs::metadata(&resolved).await.map_err(|e| {
            // Don't leak the resolved filesystem path to the client.
            warn!(track = %track_id, path = %resolved.display(), error = %e, "stat failed");
            AppError::NotFound(format!("track {track_id} file missing"))
        })?;
        if !meta.is_file() {
            return Err(AppError::NotFound(format!("track {track_id} not a file")));
        }

        Ok(ResolvedStream {
            track_id,
            content_type: content_type_for(&resolved),
            size: meta.len(),
            modified: meta.modified().ok(),
            path: resolved,
        })
    }

    /// Centralised path-safety logic. Pulled out for unit testing.
    fn resolve_file_path(&self, raw: &str) -> Result<PathBuf> {
        let candidate = PathBuf::from(raw);

        let joined = match (&self.library_root, candidate.is_absolute()) {
            (Some(root), false) => root.join(&candidate),
            (Some(_), true) => candidate,
            (None, true) => candidate,
            (None, false) => {
                return Err(AppError::Internal(
                    "track file_path is relative but no LIBRARY_PATH is configured".into(),
                ));
            }
        };

        // `canonicalize` requires the file to exist; that's the behaviour
        // we want — non-existent files fail closed here.
        let canonical = std::fs::canonicalize(&joined).map_err(|e| {
            warn!(path = %joined.display(), error = %e, "canonicalize failed");
            AppError::NotFound("track file missing".into())
        })?;

        if let Some(root) = &self.library_root {
            if !canonical.starts_with(root) {
                warn!(
                    canonical = %canonical.display(),
                    root = %root.display(),
                    "path traversal attempt blocked"
                );
                return Err(AppError::PermissionDenied(
                    "track file is outside the library root".into(),
                ));
            }
        }

        Ok(canonical)
    }
}

/// Conservative extension→MIME map. Anything not on the list streams as
/// `application/octet-stream` so the browser/player decides what to do.
fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("ogg" | "oga") => "audio/ogg",
        Some("opus") => "audio/ogg; codecs=opus",
        Some("m4a" | "aac") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("alac") => "audio/x-alac",
        Some("wv") => "audio/x-wavpack",
        Some("ape") => "audio/x-ape",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Transcoder hook — stubbed for a future phase.
// ---------------------------------------------------------------------------

/// Future transcode hook. Implementors take a `ResolvedStream` + target
/// codec/quality and return a byte stream. PLAN.md Phase 4 §4 calls for
/// a stub-only interface at this point.
#[async_trait::async_trait]
pub trait Transcoder: Send + Sync {
    fn can_transcode(&self, source_mime: &str, target_mime: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Empty TrackRepo — we don't need DB hits for path-safety tests.
    struct NullTrackRepo;
    #[async_trait]
    impl TrackRepo for NullTrackRepo {
        async fn create(
            &self,
            _: crate::db::models::NewTrack,
        ) -> crate::error::Result<crate::db::models::Track> {
            unimplemented!()
        }
        async fn get(
            &self,
            _: Uuid,
        ) -> crate::error::Result<Option<crate::db::models::Track>> {
            Ok(None)
        }
        async fn list_by_album(
            &self,
            _: Uuid,
        ) -> crate::error::Result<Vec<crate::db::models::Track>> {
            Ok(vec![])
        }
        async fn search(
            &self,
            _: &str,
            _: i64,
            _: i64,
        ) -> crate::error::Result<Vec<crate::db::models::Track>> {
            Ok(vec![])
        }
        async fn update(
            &self,
            _: Uuid,
            _: &str,
            _: Option<i32>,
            _: Option<i32>,
            _: &str,
        ) -> crate::error::Result<Option<crate::db::models::Track>> {
            Ok(None)
        }
        async fn find_by_file_path(
            &self,
            _: &str,
        ) -> crate::error::Result<Option<crate::db::models::Track>> {
            Ok(None)
        }
        async fn delete(&self, _: Uuid) -> crate::error::Result<()> {
            Ok(())
        }
    }

    fn svc(root: Option<PathBuf>) -> StreamingService {
        StreamingService::new(Arc::new(NullTrackRepo), root)
    }

    #[test]
    fn rejects_relative_when_no_library_root() {
        let s = svc(None);
        let err = s.resolve_file_path("library/foo.flac").unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn resolves_relative_inside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("song.flac");
        std::fs::write(&file, b"x").unwrap();

        let s = svc(Some(root.clone()));
        let r = s.resolve_file_path("a/b/song.flac").unwrap();
        assert!(r.starts_with(&root));
        assert!(r.ends_with("song.flac"));
    }

    #[test]
    fn rejects_traversal_via_dotdot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        // file lives next to root, not inside it
        let outside = root.parent().unwrap().join("outside.flac");
        std::fs::write(&outside, b"x").unwrap();
        let inside_root = root.join("inside");
        std::fs::create_dir_all(&inside_root).unwrap();

        let s = svc(Some(root));
        let err = s
            .resolve_file_path(&format!("inside/../../{}", outside.file_name().unwrap().to_string_lossy()))
            .unwrap_err();
        let _ = std::fs::remove_file(&outside);
        assert!(matches!(err, AppError::PermissionDenied(_)), "got {err:?}");
    }

    #[test]
    fn missing_file_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let s = svc(Some(root));
        let err = s.resolve_file_path("does/not/exist.flac").unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn content_type_mapping() {
        assert_eq!(content_type_for(Path::new("x.flac")), "audio/flac");
        assert_eq!(content_type_for(Path::new("x.MP3")), "audio/mpeg");
        assert_eq!(content_type_for(Path::new("x.opus")), "audio/ogg; codecs=opus");
        assert_eq!(
            content_type_for(Path::new("x.unknown")),
            "application/octet-stream"
        );
    }
}
