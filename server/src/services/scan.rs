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
use crate::services::storage::StorageService;
use crate::services::tag;
use super::duration;

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanReport {
    pub tracks_added: u64,
    pub tracks_skipped: u64,
    pub errors: u64,
}

/// Report from a `refresh_durations` run.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct DurationRefreshReport {
    pub total: u64,
    pub corrected: u64,
    pub skipped_missing: u64,
    pub errors: u64,
}

#[derive(Clone)]
pub struct ScanService {
    pub library: LibraryService,
    pub default_root: Option<PathBuf>,
    /// Storage accounting. When set, scans recompute the library-storage stats
    /// at the end, and uploads/the watcher can trigger a cheap aggregate
    /// refresh. Optional so the service stays constructible without it.
    pub storage: Option<StorageService>,
}

impl ScanService {
    pub fn new(library: LibraryService, default_root: Option<PathBuf>) -> Self {
        Self {
            library,
            default_root,
            storage: None,
        }
    }

    /// Wire in the storage accounting service (recompute on scan / upload).
    pub fn with_storage(mut self, storage: StorageService) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Recompute every storage stat (per-entity rollups + disk walk). No-op
    /// when storage accounting isn't wired. Logs and swallows errors — storage
    /// stats are best-effort and must never fail a scan.
    pub async fn recompute_storage_full(&self) {
        if let Some(s) = &self.storage {
            if let Err(e) = s.recompute_all().await {
                warn!(error = %e, "scan: storage recompute failed");
            }
        }
    }

    /// Cheap aggregate recompute (SQL only) — the per-upload path.
    pub async fn recompute_storage_aggregates(&self) {
        if let Some(s) = &self.storage {
            if let Err(e) = s.recompute_aggregates().await {
                warn!(error = %e, "scan: storage aggregate recompute failed");
            }
        }
    }

    /// Remove track rows whose backing file no longer exists on disk — the
    /// "drop missing rows" half of the 24h light refresh. Manager+ only.
    ///
    /// Safety guard: if a configured `library_root` is not currently a readable
    /// directory (e.g. an unmounted volume), pruning is skipped entirely so a
    /// transient mount failure can't wipe the catalog. Individual deleted
    /// files/albums are still pruned when the root is healthy.
    pub async fn prune_missing(&self, caller: &Identity) -> Result<u64> {
        caller.require(PermissionLevel::Manager)?;
        if let Some(root) = &self.library.library_root {
            if !root.is_dir() {
                warn!(root = %root.display(), "prune_missing: library root unavailable; skipping");
                return Ok(0);
            }
        }
        let rows = self.library.tracks.list_all_ids_paths().await?;
        let mut removed: u64 = 0;
        for row in &rows {
            let path = Path::new(&row.file_path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(root) = &self.library.library_root {
                root.join(path)
            } else {
                // Relative path with no root to resolve against — can't verify
                // existence, so leave it alone.
                continue;
            };
            if !resolved.exists() {
                self.library.tracks.delete(row.id).await?;
                removed += 1;
            }
        }
        Ok(removed)
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
            // Shared audio gate: skips non-audio extensions *and* macOS
            // AppleDouble sidecars (`._song.flac`) that would otherwise be
            // indexed as ghost "Unknown Artist" tracks.
            if !tag::is_audio_file(path) {
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
        // Refresh storage stats now that the catalog has changed.
        self.recompute_storage_full().await;
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

        let mut info = tag::read_tags(path)?;

        // Cross-validate duration (same logic as index_file).
        if let Some(actual) = duration::measure_duration(path) {
            let actual_ms = actual.as_millis() as i64;
            let diff = (info.duration_ms - actual_ms).abs();
            let threshold = (info.duration_ms.max(1) / 100).max(500);
            if diff > threshold {
                tracing::info!(
                    path = %path.display(),
                    tag_ms = info.duration_ms,
                    actual_ms,
                    "duration corrected during scan"
                );
                info.duration_ms = actual_ms;
            }
        }

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
            sample_rate_hz: info.sample_rate_hz,
            bit_depth: info.bit_depth,
            channels: info.channels,
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

        let mut info = tag::read_tags(path)?;

        // Cross-validate tag-reported duration against actual audio frame
        // count.  VBR MP3 without Xing header can report a duration that's
        // off by minutes; this corrects it to the real playable length.
        if let Some(actual) = duration::measure_duration(path) {
            let actual_ms = actual.as_millis() as i64;
            let diff = (info.duration_ms - actual_ms).abs();
            // Use measured duration when they differ by >1% AND >500 ms.
            // Small discrepancies (rounding, different frame-boundary
            // conventions) are expected and not worth logging.
            let threshold = (info.duration_ms.max(1) / 100).max(500);
            if diff > threshold {
                tracing::info!(
                    path = %path.display(),
                    tag_ms = info.duration_ms,
                    actual_ms,
                    diff_ms = diff,
                    "duration corrected: tag was inaccurate"
                );
                info.duration_ms = actual_ms;
            }
        }

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
            sample_rate_hz: info.sample_rate_hz,
            bit_depth: info.bit_depth,
            channels: info.channels,
            metadata_json: "{}".to_string(),
        };
        let track = self.library.create_track(caller, new).await?;
        debug!(path = %path.display(), track_id = %track.id, "index_file: indexed");
        Ok(Some(track.id))
    }

    /// Index a single audio file into an **already-resolved** album/artist.
    ///
    /// Unlike [`index_file`], this does **not** re-derive the artist/album from
    /// the file's own tags — the caller supplies the ids. This is what lets
    /// folder-grouped ingest ([`crate::services::ingest::IngestService::organize_dir`])
    /// pin every track in a folder to the one album it decided on, instead of
    /// each file minting its own album from its (possibly inconsistent) tags.
    ///
    /// Returns `Ok(None)` when the file was already indexed (same `file_path`),
    /// or `Ok(Some(track_id))` on a fresh insert.
    pub async fn index_file_into(
        &self,
        caller: &Identity,
        path: &Path,
        album_id: uuid::Uuid,
        artist_id: uuid::Uuid,
    ) -> Result<Option<uuid::Uuid>> {
        let file_path = path.to_string_lossy().to_string();

        if self
            .library
            .tracks
            .find_by_file_path(&file_path)
            .await?
            .is_some()
        {
            debug!(path = %path.display(), "index_file_into: already indexed, skipping");
            return Ok(None);
        }

        let mut info = tag::read_tags(path)?;
        info.duration_ms = reconcile_duration(path, info.duration_ms);

        let new = NewTrack {
            album_id,
            artist_id,
            title: info.title,
            track_no: info.track_no,
            disc_no: info.disc_no,
            duration_ms: info.duration_ms,
            codec: info.codec,
            bitrate_kbps: info.bitrate_kbps,
            file_path,
            file_size: info.file_size,
            sample_rate_hz: info.sample_rate_hz,
            bit_depth: info.bit_depth,
            channels: info.channels,
            metadata_json: "{}".to_string(),
        };
        let track = self.library.create_track(caller, new).await?;
        debug!(path = %path.display(), track_id = %track.id, "index_file_into: indexed");
        Ok(Some(track.id))
    }

    /// Refresh the duration of every track in the library.
    ///
    /// Walks all tracks in the DB, opens each file, measures actual audio
    /// duration via Symphonia, and updates the DB row when the measured
    /// value differs from the stored one.  Manager+ only.
    pub async fn refresh_durations(
        &self,
        caller: &Identity,
    ) -> Result<DurationRefreshReport> {
        caller.require(PermissionLevel::Manager)?;

        let rows = self.library.tracks.list_all_ids_paths().await?;
        let total = rows.len() as u64;
        let mut corrected: u64 = 0;
        let mut skipped_missing: u64 = 0;
        let mut errors: u64 = 0;

        for row in &rows {
            let path = std::path::Path::new(&row.file_path);
            // Resolve relative paths against the library root.
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(root) = &self.library.library_root {
                root.join(path)
            } else {
                skipped_missing += 1;
                continue;
            };

            if !resolved.is_file() {
                skipped_missing += 1;
                continue;
            }

            match duration::measure_duration(&resolved) {
                Some(actual) => {
                    let actual_ms = actual.as_millis() as i64;
                    let diff = (row.duration_ms - actual_ms).abs();
                    // Same threshold as index_file: >1% and >500ms.
                    let threshold = (row.duration_ms.max(1) / 100).max(500);
                    if diff > threshold {
                        if let Err(e) = self
                            .library
                            .tracks
                            .update_duration(row.id, actual_ms)
                            .await
                        {
                            tracing::warn!(
                                id = %row.id,
                                error = %e,
                                "refresh_durations: update failed"
                            );
                            errors += 1;
                        } else {
                            tracing::info!(
                                id = %row.id,
                                tag_ms = row.duration_ms,
                                actual_ms,
                                "refresh_durations: corrected"
                            );
                            corrected += 1;
                        }
                    }
                }
                None => {
                    // Format not supported by Symphonia — skip.
                    skipped_missing += 1;
                }
            }
        }

        let report = DurationRefreshReport {
            total,
            corrected,
            skipped_missing,
            errors,
        };
        tracing::info!(
            total,
            corrected,
            skipped_missing,
            errors,
            "refresh_durations: complete"
        );
        Ok(report)
    }

    /// Rescan every track: re-read file tags (title, track_no, disc_no,
    /// codec, bitrate) AND re-measure duration via Symphonia.  Manager+ only.
    pub async fn rescan_library(
        &self,
        caller: &Identity,
        full_metadata: bool,
    ) -> Result<DurationRefreshReport> {
        caller.require(PermissionLevel::Manager)?;

        let rows = self.library.tracks.list_all_ids_paths().await?;
        let total = rows.len() as u64;
        let mut corrected: u64 = 0;
        let mut skipped_missing: u64 = 0;
        let mut errors: u64 = 0;

        for row in &rows {
            let path = std::path::Path::new(&row.file_path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else if let Some(root) = &self.library.library_root {
                root.join(path)
            } else {
                skipped_missing += 1;
                continue;
            };

            if !resolved.is_file() {
                skipped_missing += 1;
                continue;
            }

            let mut changed = false;

            // Re-measure actual playback duration via Symphonia.
            if let Some(actual) = super::duration::measure_duration(&resolved) {
                let actual_ms = actual.as_millis() as i64;
                let diff = (row.duration_ms - actual_ms).abs();
                let threshold = (row.duration_ms.max(1) / 100).max(500);
                if diff > threshold {
                    if let Err(e) = self
                        .library
                        .tracks
                        .update_duration(row.id, actual_ms)
                        .await
                    {
                        tracing::warn!(id=%row.id, error=%e, "rescan: duration update failed");
                        errors += 1;
                        continue;
                    }
                    changed = true;
                }
            }

            // Re-read tags for full metadata refresh.
            if full_metadata {
                match tag::read_tags(&resolved) {
                    Ok(info) => {
                        // Update tag-derived fields (title, track_no, disc_no).
                        // Duration is handled above via Symphonia measurement.
                        if let Err(e) = self
                            .library
                            .tracks
                            .update(
                                row.id,
                                &info.title,
                                info.track_no,
                                info.disc_no,
                                "{}",
                            )
                            .await
                        {
                            tracing::warn!(id=%row.id, error=%e, "rescan: tag update failed");
                            errors += 1;
                        } else {
                            changed = true;
                        }
                        // Refresh the file-derived technical fields too so a
                        // rescan repairs stale codec/bitrate/size as well as
                        // duration.
                        if let Err(e) = self
                            .library
                            .tracks
                            .update_file_props(
                                row.id,
                                &info.codec,
                                info.bitrate_kbps,
                                info.file_size,
                                info.sample_rate_hz,
                                info.bit_depth,
                                info.channels,
                            )
                            .await
                        {
                            tracing::warn!(id=%row.id, error=%e, "rescan: file-props update failed");
                            errors += 1;
                        } else {
                            changed = true;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(id=%row.id, error=%e, "rescan: tag read failed");
                        errors += 1;
                    }
                }
            }

            if changed {
                corrected += 1;
            }
        }

        let report = DurationRefreshReport {
            total,
            corrected,
            skipped_missing,
            errors,
        };
        tracing::info!(total, corrected, skipped_missing, errors, "rescan_library: complete");
        // A rescan can change file sizes (re-tagged/replaced files), so refresh
        // the storage stats too.
        self.recompute_storage_full().await;
        Ok(report)
    }
}

/// Cross-validate a tag-reported duration against the actual decoded length.
///
/// Returns the measured duration when it diverges from the tag value by more
/// than 1% **and** 500 ms (the same threshold used inline by [`ScanService::index_file`]);
/// otherwise the tag value is kept. Falls back to the tag value when the
/// format isn't measurable by Symphonia.
fn reconcile_duration(path: &Path, tag_ms: i64) -> i64 {
    let Some(actual) = duration::measure_duration(path) else {
        return tag_ms;
    };
    let actual_ms = actual.as_millis() as i64;
    let diff = (tag_ms - actual_ms).abs();
    let threshold = (tag_ms.max(1) / 100).max(500);
    if diff > threshold {
        tracing::info!(
            path = %path.display(),
            tag_ms,
            actual_ms,
            diff_ms = diff,
            "duration corrected: tag was inaccurate"
        );
        actual_ms
    } else {
        tag_ms
    }
}
