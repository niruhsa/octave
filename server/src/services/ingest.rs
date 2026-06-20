//! Upload & ingest service.
//!
//! Orchestrates the full ingest pipeline used by both the REST upload
//! endpoint and the background folder watcher.
//!
//! All file operations are **copy-only** — sources are never moved or deleted.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tracing::debug;
use uuid::Uuid;

use crate::auth::Identity;
use crate::error::{AppError, Result};
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
