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

/// Fallback identity labels, kept in sync with [`crate::services::tag`]'s
/// per-file defaults so folder-grouped ingest can recognise (and skip) them
/// when deciding a single album identity for a whole directory.
const UNKNOWN_ARTIST: &str = "Unknown Artist";
const UNKNOWN_ALBUM: &str = "Unknown Album";
/// Album-artist used when a folder holds tracks by two or more distinct
/// artists (a compilation), matching the common MusicBrainz convention.
const VARIOUS_ARTISTS: &str = "Various Artists";

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
    /// Optional acoustic fingerprinting (Phase 12). When set, every
    /// newly-indexed track is analyzed in the background on ingest so "sounds
    /// like" radio works promptly for fresh uploads (the periodic pass is the
    /// catch-all). `None` when `FINGERPRINT_ENABLED` is off.
    pub fingerprint: Option<crate::services::FingerprintService>,
    /// Optional lyrics service (Phase 15). When set, every newly-indexed track
    /// gets a background lyric resolve (sidecar → embedded → LRCLIB) on ingest,
    /// beside the artwork + fingerprint hooks. `None` when `LYRICS_ENABLED` is
    /// off.
    pub lyrics: Option<crate::services::LyricsService>,
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
            fingerprint: None,
            lyrics: None,
        }
    }

    /// Attach the fingerprint service so new uploads are analyzed on ingest.
    pub fn with_fingerprint(
        mut self,
        fingerprint: Option<crate::services::FingerprintService>,
    ) -> Self {
        self.fingerprint = fingerprint;
        self
    }

    /// Attach the lyrics service so new uploads get lyrics resolved on ingest.
    pub fn with_lyrics(mut self, lyrics: Option<crate::services::LyricsService>) -> Self {
        self.lyrics = lyrics;
        self
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

        // Fingerprint a genuinely-new track in the background (Phase 12) so
        // acoustic "sounds like" works promptly without blocking the upload.
        // Re-ingests (`already_indexed`) are left to the periodic freshness
        // pass, which re-analyzes only when the file signature actually changed.
        if !already_indexed {
            self.spawn_fingerprint(track_id);
            self.spawn_lyrics(track_id);
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

    /// Ingest every audio file directly inside `dir` as a **single album**.
    ///
    /// This is the folder-grouped counterpart to [`organize_and_index`] and is
    /// the entry point used by the watcher, the REST ingest scan, and archive
    /// extraction. It fixes the "one album per file" fragmentation: instead of
    /// each file resolving (and often minting) its own album from its own tags,
    /// one artist + album is chosen for the whole folder and **every** track is
    /// pinned to it. A folder `cover.jpg` (or other sidecar) becomes the album
    /// art; non-audio files never create anything.
    ///
    /// Only the immediate children of `dir` are grouped — a nested
    /// `Language/Artist/Album/Track.flac` tree still yields one album per
    /// leaf `Album` folder, since each track's parent directory is its album.
    pub async fn organize_dir(
        &self,
        caller: &Identity,
        dir: &Path,
    ) -> Result<DirIngestResult> {
        let mut files = collect_audio_files(dir);
        files.sort();

        let mut result = DirIngestResult::default();
        if files.is_empty() {
            return Ok(result);
        }

        // Read every file's tags up front so we can decide one identity for the
        // whole folder before touching the catalog.
        let mut tagged: Vec<(PathBuf, tag::TagInfo)> = Vec::with_capacity(files.len());
        for file in files {
            match tag::read_tags(&file) {
                Ok(info) => tagged.push((file, info)),
                Err(e) => {
                    warn!(path = %file.display(), error = %e, "ingest dir: tag read failed");
                    result.errors += 1;
                }
            }
        }
        if tagged.is_empty() {
            return Ok(result);
        }

        let infos: Vec<&tag::TagInfo> = tagged.iter().map(|(_, i)| i).collect();
        let album_artist = choose_album_artist(&infos);
        let album_title = choose_album_title(&infos, dir);
        let language = choose_language(&infos, &album_artist);
        let year = choose_year(&infos);
        drop(infos);

        // Upsert the single artist + album that the whole folder maps onto.
        let artist = match self
            .scan
            .library
            .artists
            .find_by_name(&album_artist)
            .await?
        {
            Some(a) => a,
            None => {
                self.scan
                    .library
                    .create_artist(caller, &album_artist, None)
                    .await?
            }
        };
        let album = match self
            .scan
            .library
            .albums
            .find_by_artist_and_title(artist.id, &album_title)
            .await?
        {
            Some(a) => a,
            None => {
                self.scan
                    .library
                    .create_album(caller, artist.id, &album_title, year, None)
                    .await?
            }
        };
        let needs_cover = album.cover_path.is_none();

        // Organise + index each track into that one album. Every track keeps
        // its own title / track number but is forced under the shared
        // language/artist/album folder so the files live together on disk.
        let mut first_placed: Option<(PathBuf, PathBuf)> = None; // (source, dest)
        for (source, mut info) in tagged {
            info.artist = album_artist.clone();
            info.album = album_title.clone();
            info.language = language.clone();

            let dest = match self.organizer.organize(&source, &info) {
                Ok(d) => d,
                Err(e) => {
                    warn!(path = %source.display(), error = %e, "ingest dir: organize failed");
                    result.errors += 1;
                    continue;
                }
            };

            match self
                .scan
                .index_file_into(caller, &dest, album.id, artist.id)
                .await
            {
                Ok(Some(track_id)) => {
                    result.ingested += 1;
                    result.track_ids.push(track_id);
                    self.spawn_fingerprint(track_id);
                    self.spawn_lyrics(track_id);
                }
                Ok(None) => {
                    result.already_indexed += 1;
                    if let Ok(Some(existing)) = self
                        .scan
                        .library
                        .tracks
                        .find_by_file_path(&dest.to_string_lossy())
                        .await
                    {
                        result.track_ids.push(existing.id);
                    }
                }
                Err(e) => {
                    warn!(path = %dest.display(), error = %e, "ingest dir: index failed");
                    result.errors += 1;
                    continue;
                }
            }

            if first_placed.is_none() {
                first_placed = Some((source, dest));
            }
        }

        // Cover art for a newly-created album. Prefer art that shipped with the
        // folder — a `cover.jpg` sidecar (or art embedded in the first track) —
        // so user-supplied artwork wins over a remote lookup. `local_cover`
        // scans the source's parent dir (== `dir`) for the sidecar.
        if needs_cover
            && let Some((source, dest)) = &first_placed
        {
            if let Some(cover) = local_cover(source) {
                match self
                    .write_cover_to_library(caller, &album, dest, &cover)
                    .await
                {
                    Ok(path) => tracing::info!(
                        album_id = %album.id,
                        cover_path = %path,
                        "artwork: wrote folder cover into library"
                    ),
                    Err(e) => tracing::warn!(
                        album_id = %album.id,
                        error = %e,
                        "artwork: folder cover write failed"
                    ),
                }
            } else if let Some(artwork) = &self.artwork {
                let artwork = artwork.clone();
                let caller_owned = caller.clone();
                let album_id = album.id;
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
            dir = %dir.display(),
            ingested = result.ingested,
            already = result.already_indexed,
            errors = result.errors,
            album = %album_title,
            artist = %album_artist,
            "organize_dir: complete"
        );
        Ok(result)
    }

    /// Fingerprint a genuinely-new track in the background (Phase 12) so
    /// acoustic "sounds like" works promptly without blocking ingest. No-op
    /// when fingerprinting is disabled.
    fn spawn_fingerprint(&self, track_id: Uuid) {
        if let Some(fp) = &self.fingerprint {
            let fp = fp.clone();
            tokio::spawn(async move {
                match fp.analyze_track(track_id).await {
                    Ok(()) => tracing::debug!(track_id = %track_id, "ingest: fingerprint analyzed"),
                    Err(e) => tracing::debug!(
                        track_id = %track_id,
                        error = %e,
                        "ingest: fingerprint analyze skipped/failed"
                    ),
                }
            });
        }
    }

    /// Resolve lyrics for a genuinely-new track in the background (Phase 15) so
    /// the NowPlaying panel has them promptly (the periodic pass is the
    /// catch-all). Uses a system identity for the audited write. No-op when
    /// lyrics are disabled.
    fn spawn_lyrics(&self, track_id: Uuid) {
        if let Some(lyrics) = &self.lyrics {
            let lyrics = lyrics.clone();
            tokio::spawn(async move {
                match lyrics.resolve_track(&Identity::SecretKey, track_id).await {
                    Ok(outcome) => {
                        tracing::debug!(track_id = %track_id, ?outcome, "ingest: lyrics resolved")
                    }
                    Err(e) => tracing::debug!(
                        track_id = %track_id,
                        error = %e,
                        "ingest: lyrics resolve skipped/failed"
                    ),
                }
            });
        }
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
    /// staging area, then ingest each extracted folder as one album via
    /// [`organize_dir`]. Non-audio members are ignored. The extracted temp
    /// tree is removed afterwards; the original archive is never modified.
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

        // Group the extracted members by their parent directory so each folder
        // in the archive becomes a single album (same fix as the loose-folder
        // watcher path), rather than one album per file. Non-audio members are
        // counted but never create anything.
        let mut result = ArchiveIngestResult::default();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for member in &members {
            if !tag::is_audio_file(member) {
                result.non_audio_skipped += 1;
                continue;
            }
            if let Some(parent) = member.parent() {
                let parent = parent.to_path_buf();
                if !dirs.contains(&parent) {
                    dirs.push(parent);
                }
            }
        }
        for dir in &dirs {
            match self.organize_dir(caller, dir).await {
                Ok(r) => {
                    result.ingested += r.ingested;
                    result.already_indexed += r.already_indexed;
                    result.errors += r.errors;
                    result.track_ids.extend(r.track_ids);
                }
                Err(e) => {
                    warn!(dir = %dir.display(), error = %e, "archive: dir ingest failed");
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

/// Collect the audio files that are **immediate children** of `dir`.
///
/// Applies the same gate as every other ingest path: recognised audio
/// extensions only, skipping AppleDouble sidecars (`._x.flac`, handled inside
/// [`tag::is_audio_file`]) and in-flight `.uploading` staging files. Returns an
/// empty vec (never an error) when `dir` can't be read.
fn collect_audio_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !tag::is_audio_file(&path) {
            continue;
        }
        let is_uploading = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("uploading"))
            .unwrap_or(false);
        if is_uploading {
            continue;
        }
        out.push(path);
    }
    out
}

/// Pick the album-artist for a folder from its files' tags.
///
/// One distinct (non-`Unknown`) primary artist → that artist. Two or more →
/// [`VARIOUS_ARTISTS`] (a compilation). None → [`UNKNOWN_ARTIST`].
fn choose_album_artist(infos: &[&tag::TagInfo]) -> String {
    let mut distinct: Vec<&str> = Vec::new();
    for info in infos {
        let a = info.artist.trim();
        if a.is_empty() || a.eq_ignore_ascii_case(UNKNOWN_ARTIST) {
            continue;
        }
        if !distinct.iter().any(|d| d.eq_ignore_ascii_case(a)) {
            distinct.push(a);
        }
    }
    match distinct.len() {
        0 => UNKNOWN_ARTIST.to_string(),
        1 => distinct[0].to_string(),
        _ => VARIOUS_ARTISTS.to_string(),
    }
}

/// Pick the album title for a folder: the most common non-`Unknown` album tag
/// (ties broken by first appearance), falling back to the folder's own name and
/// finally [`UNKNOWN_ALBUM`].
fn choose_album_title(infos: &[&tag::TagInfo], dir: &Path) -> String {
    let titles = infos.iter().map(|i| i.album.as_str());
    if let Some(title) = most_common(titles, |t| {
        !t.trim().is_empty() && !t.eq_ignore_ascii_case(UNKNOWN_ALBUM)
    }) {
        return title.to_string();
    }
    dir.file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNKNOWN_ALBUM.to_string())
}

/// Pick the folder's language: the most common non-empty file language,
/// falling back to inferring from the album artist's name script.
fn choose_language(infos: &[&tag::TagInfo], album_artist: &str) -> String {
    let langs = infos.iter().map(|i| i.language.as_str());
    most_common(langs, |l| !l.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::services::tag::infer_language(album_artist))
}

/// Pick the folder's release year: the most common present year (ties broken by
/// first appearance), or `None` when no file carries one.
fn choose_year(infos: &[&tag::TagInfo]) -> Option<i32> {
    let years: Vec<i32> = infos.iter().filter_map(|i| i.year).collect();
    let mut counts: Vec<(i32, usize)> = Vec::new();
    for y in years {
        if let Some(e) = counts.iter_mut().find(|(k, _)| *k == y) {
            e.1 += 1;
        } else {
            counts.push((y, 1));
        }
    }
    let mut best: Option<(i32, usize)> = None;
    for (y, n) in counts {
        if best.map_or(true, |(_, bn)| n > bn) {
            best = Some((y, n));
        }
    }
    best.map(|(y, _)| y)
}

/// Return the most common value from `iter` (case-insensitive), considering
/// only values that satisfy `keep`. Ties are broken by first appearance. The
/// returned `&str` is the first-seen spelling of the winning value.
fn most_common<'a, I, F>(iter: I, keep: F) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
    F: Fn(&str) -> bool,
{
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for v in iter {
        if !keep(v) {
            continue;
        }
        if let Some(e) = counts.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(v)) {
            e.1 += 1;
        } else {
            counts.push((v, 1));
        }
    }
    let mut best: Option<(&str, usize)> = None;
    for (v, n) in counts {
        if best.map_or(true, |(_, bn)| n > bn) {
            best = Some((v, n));
        }
    }
    best.map(|(v, _)| v)
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

/// Outcome of an `organize_dir` (folder-grouped) ingest call.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DirIngestResult {
    /// Newly-ingested tracks in this folder.
    pub ingested: u64,
    /// Tracks that were already in the library.
    pub already_indexed: u64,
    /// Files that errored during ingest.
    pub errors: u64,
    /// Track ids of every successfully-resolved track.
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

    // ---- folder-grouped album-identity selection -------------------------

    /// Minimal `TagInfo` builder for the identity-selection tests — only the
    /// fields those helpers read (artist/album/language/year) matter.
    fn tags(artist: &str, album: &str, language: &str, year: Option<i32>) -> tag::TagInfo {
        tag::TagInfo {
            title: "t".into(),
            artist: artist.into(),
            artist_raw: artist.into(),
            album: album.into(),
            language: language.into(),
            track_no: None,
            disc_no: None,
            year,
            duration_ms: 0,
            bitrate_kbps: None,
            codec: "flac".into(),
            file_size: None,
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
        }
    }

    fn refs(v: &[tag::TagInfo]) -> Vec<&tag::TagInfo> {
        v.iter().collect()
    }

    #[test]
    fn album_artist_single_when_consistent() {
        let v = vec![
            tags("Pink Floyd", "DSOTM", "English", Some(1973)),
            tags("Pink Floyd", "DSOTM", "English", Some(1973)),
        ];
        assert_eq!(choose_album_artist(&refs(&v)), "Pink Floyd");
    }

    #[test]
    fn album_artist_ignores_unknown_and_case() {
        // A tagless track ("Unknown Artist") mixed with a real one must not
        // trip the "multiple artists → Various" rule.
        let v = vec![
            tags("Unknown Artist", "DSOTM", "English", None),
            tags("pink floyd", "DSOTM", "English", None),
            tags("Pink Floyd", "DSOTM", "English", None),
        ];
        assert_eq!(choose_album_artist(&refs(&v)), "pink floyd");
    }

    #[test]
    fn album_artist_various_for_compilation() {
        let v = vec![
            tags("Artist A", "Best Of 2020", "English", None),
            tags("Artist B", "Best Of 2020", "English", None),
        ];
        assert_eq!(choose_album_artist(&refs(&v)), VARIOUS_ARTISTS);
    }

    #[test]
    fn album_artist_unknown_when_all_tagless() {
        let v = vec![tags("Unknown Artist", "Unknown Album", "English", None)];
        assert_eq!(choose_album_artist(&refs(&v)), UNKNOWN_ARTIST);
    }

    #[test]
    fn album_title_most_common_wins() {
        let v = vec![
            tags("A", "Deluxe Edition", "English", None),
            tags("A", "Deluxe Edition", "English", None),
            tags("A", "Deluxe", "English", None),
        ];
        assert_eq!(
            choose_album_title(&refs(&v), Path::new("/x/Folder")),
            "Deluxe Edition"
        );
    }

    #[test]
    fn album_title_falls_back_to_folder_name() {
        // All tracks tagless → use the folder name so the album isn't "Unknown".
        let v = vec![
            tags("A", "Unknown Album", "English", None),
            tags("A", "", "English", None),
        ];
        assert_eq!(
            choose_album_title(&refs(&v), Path::new("/music/Discovery")),
            "Discovery"
        );
    }

    #[test]
    fn year_picks_most_common_present() {
        let v = vec![
            tags("A", "X", "English", None),
            tags("A", "X", "English", Some(2001)),
            tags("A", "X", "English", Some(2001)),
            tags("A", "X", "English", Some(1999)),
        ];
        assert_eq!(choose_year(&refs(&v)), Some(2001));
        // No years at all → None.
        let none = vec![tags("A", "X", "English", None)];
        assert_eq!(choose_year(&refs(&none)), None);
    }

    #[test]
    fn language_mode_then_infer_fallback() {
        let v = vec![
            tags("A", "X", "English", None),
            tags("A", "X", "Japanese", None),
            tags("A", "X", "Japanese", None),
        ];
        assert_eq!(choose_language(&refs(&v), "A"), "Japanese");
    }

    #[test]
    fn collect_audio_files_filters() {
        let dir = tmpdir();
        std::fs::write(dir.join("01 - a.flac"), b"x").unwrap();
        std::fs::write(dir.join("02 - b.mp3"), b"x").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"x").unwrap(); // non-audio
        std::fs::write(dir.join("notes.txt"), b"x").unwrap(); // non-audio
        std::fs::write(dir.join("source.flac.uploading"), b"x").unwrap(); // staging
        std::fs::write(dir.join("._a.flac"), b"x").unwrap(); // AppleDouble

        let mut found: Vec<String> = collect_audio_files(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, vec!["01 - a.flac", "02 - b.mp3"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
