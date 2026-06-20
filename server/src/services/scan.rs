//! Library scan: walk a directory, probe audio files via `lofty`, and
//! upsert artists/albums/tracks into the catalog.
//!
//! Phase 3 keeps this intentionally simple — Phase 6 (Uploads & Ingest) is
//! where the copy-only watcher and archive formats land.

use std::path::{Path, PathBuf};

use tracing::{debug, warn};
use walkdir::WalkDir;

use crate::auth::Identity;
use crate::db::models::{NewTrack, PermissionLevel};
use crate::error::{AppError, Result};
use crate::services::library::LibraryService;
use crate::services::tag::{self, AUDIO_EXTS};

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanReport {
    pub tracks_added: u64,
    pub tracks_skipped: u64,
    pub errors: u64,
}

#[derive(Clone)]
pub struct ScanService {
    pub library: LibraryService,
    pub default_root: Option<PathBuf>,
}

impl ScanService {
    pub fn new(library: LibraryService, default_root: Option<PathBuf>) -> Self {
        Self {
            library,
            default_root,
        }
    }

    /// Scan `root` (or the configured `LIBRARY_PATH` fallback) and ingest
    /// every supported audio file. Manager+ only.
    pub async fn scan(&self, caller: &Identity, root: Option<&Path>) -> Result<ScanReport> {
        caller.require(PermissionLevel::Manager)?;

        let root = match root {
            Some(p) => p.to_path_buf(),
            None => self
                .default_root
                .clone()
                .ok_or_else(|| AppError::InvalidArgument(
                    "no scan root provided and LIBRARY_PATH is unset".into(),
                ))?,
        };
        if !root.is_dir() {
            return Err(AppError::InvalidArgument(format!(
                "{} is not a directory",
                root.display()
            )));
        }

        let mut report = ScanReport::default();
        for entry in WalkDir::new(&root).follow_links(false).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            if !matches!(ext.as_deref(), Some(e) if AUDIO_EXTS.contains(&e)) {
                continue;
            }

            match self.ingest_one(caller, path).await {
                Ok(true) => report.tracks_added += 1,
                Ok(false) => report.tracks_skipped += 1,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "scan: failed to ingest");
                    report.errors += 1;
                }
            }
        }
        Ok(report)
    }

    /// Returns `Ok(true)` if a new track row was inserted, `Ok(false)` if the
    /// file was already indexed.
    async fn ingest_one(&self, caller: &Identity, path: &Path) -> Result<bool> {
        let file_path = path.to_string_lossy().to_string();

        // Skip if already indexed.
        if self
            .library
            .tracks
            .find_by_file_path(&file_path)
            .await?
            .is_some()
        {
            return Ok(false);
        }

        let info = tag::read_tags(path)?;

        // Upsert artist (find-by-name, else create).
        let artist = match self.library.artists.find_by_name(&info.artist).await? {
            Some(a) => a,
            None => self
                .library
                .create_artist(caller, &info.artist, None)
                .await?,
        };
        // Upsert album (find by artist+title, else create).
        let album = match self
            .library
            .albums
            .find_by_artist_and_title(artist.id, &info.album)
            .await?
        {
            Some(a) => a,
            None => {
                self.library
                    .create_album(caller, artist.id, &info.album, info.year, None)
                    .await?
            }
        };

        let new = NewTrack {
            album_id: album.id,
            artist_id: artist.id,
            title: info.title,
            track_no: info.track_no,
            disc_no: info.disc_no,
            duration_ms: info.duration_ms,
            codec: info.codec,
            bitrate_kbps: info.bitrate_kbps,
            file_path: file_path,
            file_size: info.file_size,
            metadata_json: "{}".to_string(),
        };
        self.library.create_track(caller, new).await?;
        debug!(path = %path.display(), "scan: indexed");
        Ok(true)
    }

    /// Index a single audio file into the library.
    ///
    /// This is the public entry-point used by uploads and the ingest-folder
    /// watcher after they have already copied the file into its final
    /// organised location.  The file is **not** moved or renamed — it must
    /// already live at its permanent path.
    ///
    /// Returns `Ok(None)` when the file was already indexed (same
    /// `file_path`), or `Ok(Some(track_id))` on a fresh insert.  Errors
    /// bubble up for the caller to handle (bad probe, missing FK, etc.).
    pub async fn index_file(
        &self,
        caller: &Identity,
        path: &Path,
    ) -> Result<Option<uuid::Uuid>> {
        let file_path = path.to_string_lossy().to_string();

        if self
            .library
            .tracks
            .find_by_file_path(&file_path)
            .await?
            .is_some()
        {
            debug!(path = %path.display(), "index_file: already indexed, skipping");
            return Ok(None);
        }

        let info = tag::read_tags(path)?;

        let artist = match self.library.artists.find_by_name(&info.artist).await? {
            Some(a) => a,
            None => self.library.create_artist(caller, &info.artist, None).await?,
        };
        let album = match self
            .library
            .albums
            .find_by_artist_and_title(artist.id, &info.album)
            .await?
        {
            Some(a) => a,
            None => {
                self.library
                    .create_album(caller, artist.id, &info.album, info.year, None)
                    .await?
            }
        };

        let new = NewTrack {
            album_id: album.id,
            artist_id: artist.id,
            title: info.title,
            track_no: info.track_no,
            disc_no: info.disc_no,
            duration_ms: info.duration_ms,
            codec: info.codec,
            bitrate_kbps: info.bitrate_kbps,
            file_path,
            file_size: info.file_size,
            metadata_json: "{}".to_string(),
        };
        let track = self.library.create_track(caller, new).await?;
        debug!(path = %path.display(), track_id = %track.id, "index_file: indexed");
        Ok(Some(track.id))
    }
}
