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

use std::sync::Arc;

use crate::auth::Identity;
use crate::error::{AppError, Result};
use crate::services::archive::{self, ArchiveKind};
use crate::services::artwork::{ArtworkService, CoverImage};
use crate::services::organizer::Organizer;
use crate::services::scan::ScanService;
use crate::services::tag;

/// Sidecar image filenames (case-insensitive, without extension) treated as
/// album cover art when found next to an ingested audio file.
const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "albumart", "artwork"];
/// Image extensions recognised for sidecar covers.
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Top-level ingest coordinator.
#[derive(Clone)]
pub struct IngestService {
    pub scan: ScanService,
    pub organizer: Organizer,
    pub ingest_root: Option<PathBuf>,
    /// Optional artwork service for auto-fetching covers for newly ingested
    /// albums. When set, every new album created during ingest triggers a
    /// background cover lookup + embed into all track files.
    pub artwork: Option<Arc<ArtworkService>>,
}

impl IngestService {
    pub fn new(
        scan: ScanService,
        organizer: Organizer,
        ingest_root: Option<PathBuf>,
        artwork: Option<Arc<ArtworkService>>,
    ) -> Self {
        Self {
            scan,
            organizer,
            ingest_root,
            artwork,
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
        let album = match self
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

        let needs_cover = album.cover_path.is_none();

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

        // Cover art for a newly-created album. Prefer a cover that shipped
        // with the upload (sidecar `cover.jpg` next to the source, or art
        // embedded in the audio file) so user-supplied artwork wins. The
        // cover is written as `cover.<ext>` **into the library album folder**
        // (next to the tracks) and the album row's `cover_path` is pointed
        // there — this works regardless of `FETCH_ARTWORK`. Only when no
        // local cover exists do we fall back to a remote CAA lookup, and
        // that path requires the (optional) artwork service.
        if needs_cover {
            let album_id = album.id;
            if let Some(cover) = local_cover(source) {
                match self
                    .write_cover_to_library(caller, &album, &dest, &cover)
                    .await
                {
                    Ok(path) => tracing::info!(
                        album_id = %album_id,
                        cover_path = %path,
                        "artwork: wrote local cover into library folder"
                    ),
                    Err(e) => tracing::warn!(
                        album_id = %album_id,
                        error = %e,
                        "artwork: local cover write failed"
                    ),
                }
            } else if let Some(artwork) = &self.artwork {
                // No local cover — spawn a background remote fetch + embed.
                let artwork = artwork.clone();
                let caller_owned = caller.clone();
                tokio::spawn(async move {
                    match artwork.fetch_for_album(&caller_owned, album_id).await {
                        Ok(Some(path)) => tracing::info!(
                            album_id = %album_id,
                            cover_path = %path,
                            "artwork: auto-fetched during ingest"
                        ),
                        Ok(None) => tracing::debug!(
                            album_id = %album_id,
                            "artwork: no cover found for album"
                        ),
                        Err(e) => tracing::warn!(
                            album_id = %album_id,
                            error = %e,
                            "artwork: auto-fetch failed"
                        ),
                    }
                });
            }
        }

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

    /// Write a captured cover image into the **library album folder** (next
    /// to the tracks) as `cover.<ext>`, point the album row's `cover_path`
    /// there (audited via `update_album`), and embed the art into every
    /// track file in the album so it travels with the files.
    ///
    /// `track_dest` is the just-organised track path; its parent directory is
    /// the album folder. Works regardless of `FETCH_ARTWORK` since it doesn't
    /// touch the remote artwork service.
    async fn write_cover_to_library(
        &self,
        caller: &Identity,
        album: &crate::db::models::Album,
        track_dest: &Path,
        cover: &CoverImage,
    ) -> Result<String> {
        let album_dir = track_dest.parent().ok_or_else(|| {
            AppError::Internal(format!(
                "track dest has no parent dir: {}",
                track_dest.display()
            ))
        })?;

        let cover_path = album_dir.join(format!("cover.{}", cover.ext()));
        tokio::fs::write(&cover_path, &cover.bytes)
            .await
            .map_err(AppError::Io)?;
        let cover_path_str = cover_path.to_string_lossy().into_owned();

        // Point the album row at the on-disk cover (audited).
        self.scan
            .library
            .update_album(
                caller,
                album.id,
                &album.title,
                album.release_year,
                Some(&cover_path_str),
            )
            .await?;

        // Embed the cover into every track file of the album (best-effort).
        let tracks = self
            .scan
            .library
            .list_tracks_by_album(caller, album.id)
            .await?;
        for t in &tracks {
            let raw = Path::new(&t.file_path);
            let resolved = if raw.is_absolute() {
                raw.to_path_buf()
            } else if let Some(root) = &self.scan.library.library_root {
                root.join(raw)
            } else {
                continue;
            };
            if !resolved.is_file() {
                continue;
            }
            if let Err(e) = tag::write_cover(&resolved, &cover.bytes, &cover.content_type) {
                tracing::warn!(
                    track = %t.id,
                    path = %resolved.display(),
                    error = %e,
                    "ingest: embed cover failed"
                );
            }
        }

        Ok(cover_path_str)
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

/// Find a cover image shipping with an ingested audio file.
///
/// Resolution order:
///   1. **Sidecar image** — a `cover.jpg` / `folder.png` / `front.*` (etc.)
///      in the same directory as `source`. This is how loose folders and
///      extracted archives carry album art.
///   2. **Embedded art** — a front-cover picture inside the audio file's
///      own tags.
///
/// Returns `None` when neither is present.
fn local_cover(source: &Path) -> Option<CoverImage> {
    // 1. Sidecar image next to the source file.
    if let Some(dir) = source.parent() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase());
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase());
                let (Some(stem), Some(ext)) = (stem, ext) else {
                    continue;
                };
                if COVER_STEMS.contains(&stem.as_str()) && COVER_EXTS.contains(&ext.as_str()) {
                    match std::fs::read(&path) {
                        Ok(bytes) if !bytes.is_empty() => {
                            return Some(CoverImage {
                                bytes,
                                content_type: ext_to_mime(&ext).to_string(),
                            });
                        }
                        Ok(_) => {}
                        Err(e) => warn!(
                            path = %path.display(),
                            error = %e,
                            "ingest: sidecar cover read failed"
                        ),
                    }
                }
            }
        }
    }

    // 2. Embedded art inside the audio file.
    match tag::read_embedded_cover(source) {
        Ok(Some((bytes, mime))) if !bytes.is_empty() => Some(CoverImage {
            bytes,
            content_type: mime,
        }),
        Ok(_) => None,
        Err(e) => {
            debug!(source = %source.display(), error = %e, "ingest: embedded cover read failed");
            None
        }
    }
}

/// Map a lowercase image extension to its content-type string.
fn ext_to_mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ingest-cover-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn local_cover_finds_sidecar_jpg() {
        let dir = tmpdir();
        let audio = dir.join("01 - song.flac");
        std::fs::write(&audio, b"not really audio").unwrap();
        let mut f = std::fs::File::create(dir.join("cover.jpg")).unwrap();
        f.write_all(b"\xFF\xD8\xFF imagebytes").unwrap();

        let cover = local_cover(&audio).expect("sidecar cover found");
        assert_eq!(cover.content_type, "image/jpeg");
        assert!(!cover.bytes.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_cover_matches_folder_png_case_insensitive() {
        let dir = tmpdir();
        let audio = dir.join("track.mp3");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(dir.join("Folder.PNG"), b"\x89PNG imagebytes").unwrap();

        let cover = local_cover(&audio).expect("folder.png found");
        assert_eq!(cover.content_type, "image/png");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_cover_ignores_unrelated_and_empty() {
        let dir = tmpdir();
        let audio = dir.join("track.mp3");
        std::fs::write(&audio, b"x").unwrap();
        // unrelated image stem + an empty cover.jpg → no usable cover
        std::fs::write(dir.join("booklet.jpg"), b"data").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"").unwrap();

        assert!(local_cover(&audio).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ext_to_mime_mapping() {
        assert_eq!(ext_to_mime("jpg"), "image/jpeg");
        assert_eq!(ext_to_mime("jpeg"), "image/jpeg");
        assert_eq!(ext_to_mime("png"), "image/png");
        assert_eq!(ext_to_mime("webp"), "image/webp");
        assert_eq!(ext_to_mime("gif"), "image/gif");
        assert_eq!(ext_to_mime("bmp"), "image/jpeg");
    }
}
