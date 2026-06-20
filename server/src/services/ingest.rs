//! Upload & ingest service.
//!
//! Orchestrates the full ingest pipeline used by both the REST upload
//! endpoint and the background folder watcher.
//!
//! All file operations are **copy-only** — sources are never moved or deleted.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::Identity;
use crate::error::{AppError, Result};
use crate::services::archive::{self, ArchiveKind};
use crate::services::organizer::Organizer;
use crate::services::scan::ScanService;
use crate::services::tag;

/// Top-level ingest coordinator.
#[derive(Clone)]
pub struct IngestService {
    pub scan: ScanService,
    pub organizer: Organizer,
    pub ingest_root: Option<PathBuf>,
}

impl IngestService {
    pub fn new(scan: ScanService, organizer: Organizer, ingest_root: Option<PathBuf>) -> Self {
        Self {
            scan,
            organizer,
            ingest_root,
        }
    }

    // ------------------------------------------------------------------
    // Core pipeline — used by both REST upload and the folder watcher.
    // ------------------------------------------------------------------

    /// Read tags → upsert artist+album → copy to library → index.
    ///
    /// Returns the DB track id and the absolute organised destination.
    pub async fn organize_and_index(
        &self,
        caller: &Identity,
        source: &Path,
    ) -> Result<IngestResult> {
        if !tag::is_audio_file(source) {
            return Err(AppError::InvalidArgument(format!(
                "unsupported audio file: {}",
                source.display()
            )));
        }

        let tags = tag::read_tags(source)?;

        // Upsert artist+album via the library service so FKs exist before
        // ScanService.index_file creates the track row.
        let artist = match self.scan.library.artists.find_by_name(&tags.artist).await? {
            Some(a) => a,
            None => {
                self.scan
                    .library
                    .create_artist(caller, &tags.artist, None)
                    .await?
            }
        };
        let _album = match self
            .scan
            .library
            .albums
            .find_by_artist_and_title(artist.id, &tags.album)
            .await?
        {
            Some(a) => a,
            None => {
                self.scan
                    .library
                    .create_album(caller, artist.id, &tags.album, tags.year, None)
                    .await?
            }
        };

        let dest = self.organizer.organize(source, &tags)?;

        // index_file returns Ok(None) when the destination is already in the
        // tracks table — a normal idempotent re-ingest, not an error. Fall
        // back to a lookup so the caller still gets the existing track id.
        let (track_id, already_indexed) = match self.scan.index_file(caller, &dest).await? {
            Some(id) => (id, false),
            None => {
                let existing = self
                    .scan
                    .library
                    .tracks
                    .find_by_file_path(&dest.to_string_lossy())
                    .await?
                    .ok_or_else(|| {
                        AppError::Internal(
                            "index_file returned None but file_path lookup failed".into(),
                        )
                    })?;
                (existing.id, true)
            }
        };

        debug!(
            track_id = %track_id,
            already_indexed,
            dest = %dest.display(),
            "organize_and_index: complete"
        );

        Ok(IngestResult {
            track_id,
            dest,
            already_indexed,
        })
    }

    // ------------------------------------------------------------------
    // Archive ingest
    // ------------------------------------------------------------------

    /// Extract an archive (zip/tarball) into a temp dir under the ingest
    /// staging area, then `organize_and_index` every audio member. Non-audio
    /// members are ignored. The extracted temp tree is removed afterwards;
    /// the original archive is never modified.
    ///
    /// ISO/CD disc images are recognised but not yet supported and return
    /// `InvalidArgument` (PLAN Phase 6 stub).
    pub async fn organize_archive(
        &self,
        caller: &Identity,
        source: &Path,
        kind: ArchiveKind,
    ) -> Result<ArchiveIngestResult> {
        // Stage extraction under <ingest_root>/.tmp/<uuid> (or system temp
        // when no ingest root is configured) so the watcher's leading-dot
        // skip rule keeps the watcher from re-ingesting extracted files.
        let stage_base = self
            .ingest_root
            .as_ref()
            .map(|r| r.join(".tmp"))
            .unwrap_or_else(std::env::temp_dir);
        let stage = stage_base.join(format!("extract-{}", Uuid::new_v4()));

        let source = source.to_path_buf();
        let stage_for_blocking = stage.clone();
        // Extraction is blocking (sync std::fs + decoders) — keep it off the
        // async runtime's worker threads.
        let members = tokio::task::spawn_blocking(move || {
            archive::extract(&source, kind, &stage_for_blocking)
        })
        .await
        .map_err(|e| AppError::Internal(format!("extract task join: {e}")))??;

        let mut result = ArchiveIngestResult::default();
        for member in &members {
            if !tag::is_audio_file(member) {
                result.non_audio_skipped += 1;
                continue;
            }
            match self.organize_and_index(caller, member).await {
                Ok(r) if r.already_indexed => {
                    result.already_indexed += 1;
                    result.track_ids.push(r.track_id);
                }
                Ok(r) => {
                    result.ingested += 1;
                    result.track_ids.push(r.track_id);
                }
                Err(e) => {
                    warn!(member = %member.display(), error = %e, "archive: member ingest failed");
                    result.errors += 1;
                }
            }
        }

        // Best-effort cleanup of the staging tree.
        if let Err(e) = tokio::fs::remove_dir_all(&stage).await {
            debug!(stage = %stage.display(), error = %e, "archive: stage cleanup failed");
        }

        debug!(
            ingested = result.ingested,
            already = result.already_indexed,
            skipped = result.non_audio_skipped,
            errors = result.errors,
            "organize_archive: complete"
        );
        Ok(result)
    }
}

/// Outcome of an `organize_archive` call.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ArchiveIngestResult {
    /// Newly-ingested audio members.
    pub ingested: u64,
    /// Audio members that were already in the library.
    pub already_indexed: u64,
    /// Non-audio members ignored.
    pub non_audio_skipped: u64,
    /// Members that errored during ingest.
    pub errors: u64,
    /// Track ids of every successfully-resolved audio member.
    pub track_ids: Vec<Uuid>,
}

/// Outcome of a single `organize_and_index` call.
#[derive(Debug, Clone, Serialize)]
pub struct IngestResult {
    pub track_id: Uuid,
    pub dest: PathBuf,
    /// `true` when the destination already had a track row before this call
    /// — the copy and DB insert were both no-ops.
    pub already_indexed: bool,
}
