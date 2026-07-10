//! Lyric resolution + caching orchestration.
//!
//! Mirrors [`ArtworkService`](crate::services::artwork::ArtworkService) +
//! [`FingerprintService`](crate::services::fingerprint::FingerprintService):
//! resolve each track (sidecar → embedded tag → LRCLIB), write the `.lrc` to
//! `LYRICS_PATH`, and point the row there through [`LibraryService`] so the
//! mutation is audited. Idempotent + incremental; the background pass, the
//! on-ingest hook, and the Manager refetch/manual paths all funnel through the
//! same core.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{LyricsCandidate, LyricsMeta, PermissionLevel};
use crate::db::repo::TrackRepo;
use crate::error::{AppError, Result};
use crate::services::library::LibraryService;
use crate::services::tag;

use super::lrc::{self, LyricLine};
use super::source::{LyricQuery, LyricResult, LyricsSource};

/// Tracks resolved per background pass — bounded so a huge library is chipped
/// away incrementally at LRCLIB-polite rates instead of in one burst.
const PASS_LIMIT: i64 = 500;

/// The outcome of resolving one track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsOutcome {
    Resolved { synced: bool },
    Instrumental,
    NotFound,
    SkippedFresh,
}

/// Tally of one [`LyricsService::run_pass`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LyricsReport {
    pub resolved: u64,
    pub synced: u64,
    pub instrumental: u64,
    pub skipped: u64,
    pub failed: u64,
    pub total: u64,
}

/// Parsed lyrics for serving — what `/tracks/:id/lyrics` returns.
#[derive(Debug, Clone, Default)]
pub struct LyricsView {
    /// `true` when there are renderable lines.
    pub found: bool,
    pub synced: bool,
    pub instrumental: bool,
    pub source: Option<String>,
    pub lines: Vec<LyricLine>,
    pub plain: String,
}

/// Library-wide lyric coverage for the status endpoint.
#[derive(Debug, Clone, Copy, Default)]
pub struct LyricsStatus {
    pub synced: i64,
    pub plain: i64,
    pub instrumental: i64,
    pub missing: i64,
}

/// Which source a resolution came from, before any DB write (kept separate so
/// the priority logic is a pure, testable function without a `LibraryService`).
enum Resolution {
    Hit { text: String, source: &'static str },
    Instrumental,
    None,
}

#[derive(Clone)]
pub struct LyricsService {
    tracks: Arc<dyn TrackRepo>,
    /// Audited DB writes (the `lyrics_*` columns), like artwork's `cover_path`.
    library: LibraryService,
    /// External provider. `None` when `LYRICS_FETCH` is off → sidecar +
    /// embedded only, zero outbound calls.
    source: Option<Arc<dyn LyricsSource>>,
    /// On-disk `.lrc` cache dir (`LYRICS_PATH`).
    lyrics_root: PathBuf,
    /// Library root to resolve relative `Track.file_path`s + read sidecars/tags.
    library_root: Option<PathBuf>,
    /// Embed resolved lyrics back into the audio file's tags (`WRITE_LYRICS`).
    write_back: bool,
    concurrency: usize,
}

impl LyricsService {
    pub fn new(
        tracks: Arc<dyn TrackRepo>,
        library: LibraryService,
        source: Option<Arc<dyn LyricsSource>>,
        lyrics_root: PathBuf,
        library_root: Option<PathBuf>,
        concurrency: usize,
    ) -> Self {
        Self {
            tracks,
            library,
            source,
            lyrics_root,
            library_root,
            write_back: false,
            concurrency: concurrency.max(1),
        }
    }

    /// Enable tag write-back (`WRITE_LYRICS`).
    pub fn with_write_back(mut self, on: bool) -> Self {
        self.write_back = on;
        self
    }

    /// Whether the external provider is wired (`LYRICS_FETCH`).
    pub fn fetch_enabled(&self) -> bool {
        self.source.is_some()
    }

    // ---- serving -------------------------------------------------------------

    /// Read + parse a track's cached lyrics for the client. Never errors on a
    /// missing/instrumental track — the panel degrades gracefully.
    ///
    /// An already-uploaded track the background pass hasn't reached yet has no
    /// `lyrics_path`. Rather than show "No lyrics available" until the (slow,
    /// polite) pass gets to it, we **resolve on demand** here — as a system
    /// identity for the audited write — so opening the panel fetches lyrics
    /// immediately (like on-demand artwork optimization). A definitive
    /// instrumental result is terminal; a network miss stays retryable.
    pub async fn get(&self, caller: &Identity, track_id: Uuid) -> Result<LyricsView> {
        let mut track = self.library.get_track(caller, track_id).await?;

        if track.lyrics_path.is_none() && !track.lyrics_instrumental {
            match self
                .resolve_track_inner(&Identity::SecretKey, track_id, false)
                .await
            {
                Ok(outcome) => {
                    tracing::debug!(track = %track_id, ?outcome, "lyrics: on-demand resolve")
                }
                Err(e) => {
                    tracing::debug!(track = %track_id, error = %e, "lyrics: on-demand resolve failed")
                }
            }
            track = self.library.get_track(caller, track_id).await?;
        }

        if track.lyrics_instrumental {
            return Ok(LyricsView {
                instrumental: true,
                source: track.lyrics_source.clone(),
                ..Default::default()
            });
        }
        let Some(rel) = track.lyrics_path.as_deref() else {
            return Ok(LyricsView::default()); // pending / none
        };
        let path = self.lyrics_root.join(rel);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(track = %track_id, path = %path.display(), error = %e, "lyrics: cache file unreadable");
                return Ok(LyricsView::default());
            }
        };
        let parsed = lrc::parse(&text);
        Ok(LyricsView {
            found: !parsed.is_empty(),
            synced: parsed.synced,
            instrumental: false,
            source: track.lyrics_source.clone(),
            lines: parsed.lines,
            plain: parsed.plain,
        })
    }

    /// Library-wide coverage counts (status endpoint).
    pub async fn status(&self) -> LyricsStatus {
        match self.tracks.lyrics_counts().await {
            Ok(c) => LyricsStatus {
                synced: c.synced,
                plain: c.plain,
                instrumental: c.instrumental,
                missing: c.missing,
            },
            Err(e) => {
                tracing::warn!(error = %e, "lyrics: counts query failed");
                LyricsStatus::default()
            }
        }
    }

    // ---- resolution ----------------------------------------------------------

    /// Resolve one track (ingest hook + internal). Idempotent: a track already
    /// fresh for its `source_sig` is skipped.
    pub async fn resolve_track(&self, caller: &Identity, track_id: Uuid) -> Result<LyricsOutcome> {
        self.resolve_track_inner(caller, track_id, false).await
    }

    /// Manager: force a re-resolve (ignores the instrumental/fresh short-circuit).
    pub async fn refetch(&self, caller: &Identity, track_id: Uuid) -> Result<LyricsOutcome> {
        caller.require(PermissionLevel::Manager)?;
        self.resolve_track_inner(caller, track_id, true).await
    }

    /// Manager: set lyrics from an uploaded `.lrc`/text blob (`source = manual`).
    pub async fn set_manual(
        &self,
        caller: &Identity,
        track_id: Uuid,
        text: String,
    ) -> Result<LyricsView> {
        caller.require(PermissionLevel::Manager)?;
        let track = self.library.get_track(caller, track_id).await?;
        let audio = self.resolve_path(&track.file_path);
        let sig = audio.as_deref().and_then(source_sig).unwrap_or_default();
        match self
            .store_text(caller, track_id, &text, "manual", &sig, audio.as_deref())
            .await?
        {
            LyricsOutcome::Resolved { .. } => self.get(caller, track_id).await,
            _ => Err(AppError::InvalidArgument("no renderable lyrics in upload".into())),
        }
    }

    /// Manager: clear a track's lyrics (removes the row pointer + cache file).
    pub async fn clear(&self, caller: &Identity, track_id: Uuid) -> Result<()> {
        self.library.clear_track_lyrics(caller, track_id).await?;
        let dest = self.lyrics_root.join(format!("{track_id}.lrc"));
        let _ = tokio::fs::remove_file(dest).await;
        Ok(())
    }

    /// Background pass: resolve every pending track. Idempotent + incremental.
    pub async fn run_pass(&self) -> LyricsReport {
        let caller = Identity::SecretKey;
        let candidates = match self.tracks.lyrics_candidates(PASS_LIMIT).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "lyrics pass: listing candidates failed");
                return LyricsReport::default();
            }
        };
        let total = candidates.len() as u64;
        let results = stream::iter(candidates.into_iter().map(|c| {
            let this = self.clone();
            let caller = caller.clone();
            async move { this.resolve_candidate(&caller, c).await }
        }))
        .buffer_unordered(self.concurrency)
        .collect::<Vec<_>>()
        .await;

        let mut report = LyricsReport {
            total,
            ..Default::default()
        };
        for r in results {
            match r {
                Ok(LyricsOutcome::Resolved { synced }) => {
                    report.resolved += 1;
                    if synced {
                        report.synced += 1;
                    }
                }
                Ok(LyricsOutcome::Instrumental) => report.instrumental += 1,
                Ok(LyricsOutcome::SkippedFresh) => report.skipped += 1,
                Ok(LyricsOutcome::NotFound) => {}
                Err(_) => report.failed += 1,
            }
        }
        tracing::info!(
            resolved = report.resolved,
            synced = report.synced,
            instrumental = report.instrumental,
            failed = report.failed,
            total = report.total,
            "lyrics pass complete"
        );
        report
    }

    /// Run a pass on startup, then every `interval_secs` (0 = startup-only).
    /// Background + low priority — never blocks boot. Mirrors the fingerprint /
    /// discography pollers.
    pub fn spawn_poller(self: &Arc<Self>, interval_secs: u64) {
        let this = self.clone();
        tokio::spawn(async move {
            this.run_pass().await;
        });
        if interval_secs == 0 {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            tick.tick().await; // consume the immediate first tick (startup pass ran)
            loop {
                tick.tick().await;
                this.run_pass().await;
            }
        });
    }

    // ---- internals -----------------------------------------------------------

    async fn resolve_candidate(
        &self,
        caller: &Identity,
        c: LyricsCandidate,
    ) -> Result<LyricsOutcome> {
        let Some(path) = self.resolve_path(&c.file_path) else {
            return Err(AppError::Internal(
                "track file_path is relative but no LIBRARY_PATH is configured".into(),
            ));
        };
        let sig = source_sig(&path).unwrap_or_default();
        let duration_secs = (c.duration_ms / 1000).max(0) as u32;
        self.resolve_fields(
            caller,
            c.id,
            &path,
            &c.title,
            &c.artist,
            &c.album,
            duration_secs,
            &sig,
        )
        .await
    }

    async fn resolve_track_inner(
        &self,
        caller: &Identity,
        track_id: Uuid,
        force: bool,
    ) -> Result<LyricsOutcome> {
        let track = self.library.get_track(caller, track_id).await?;
        let Some(path) = self.resolve_path(&track.file_path) else {
            return Err(AppError::Internal(
                "track file_path is relative but no LIBRARY_PATH is configured".into(),
            ));
        };
        let sig = source_sig(&path).unwrap_or_default();
        if !force {
            let same_sig = track.lyrics_source_sig.as_deref() == Some(sig.as_str());
            if same_sig && (track.lyrics_path.is_some() || track.lyrics_instrumental) {
                return Ok(LyricsOutcome::SkippedFresh);
            }
        }
        // Artist/album display names for the provider query.
        let artist = self.library.get_artist(caller, track.artist_id).await?;
        let album = self.library.get_album(caller, track.album_id).await?;
        let duration_secs = (track.duration_ms / 1000).max(0) as u32;
        self.resolve_fields(
            caller,
            track_id,
            &path,
            &track.title,
            &artist.name,
            &album.title,
            duration_secs,
            &sig,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_fields(
        &self,
        caller: &Identity,
        track_id: Uuid,
        path: &Path,
        title: &str,
        artist: &str,
        album: &str,
        duration_secs: u32,
        sig: &str,
    ) -> Result<LyricsOutcome> {
        let q = LyricQuery {
            artist,
            title,
            album,
            duration_secs,
        };
        match resolve_source(self.source.as_ref(), path, &q).await? {
            Resolution::Hit { text, source } => {
                self.store_text(caller, track_id, &text, source, sig, Some(path))
                    .await
            }
            Resolution::Instrumental => {
                self.library
                    .set_track_lyrics_instrumental(caller, track_id, sig)
                    .await?;
                Ok(LyricsOutcome::Instrumental)
            }
            Resolution::None => Ok(LyricsOutcome::NotFound),
        }
    }

    /// Parse + cache a lyric blob and point the row at it (audited). Optionally
    /// embeds it back into the audio tags (`WRITE_LYRICS`).
    async fn store_text(
        &self,
        caller: &Identity,
        track_id: Uuid,
        text: &str,
        source: &str,
        sig: &str,
        audio_path: Option<&Path>,
    ) -> Result<LyricsOutcome> {
        let parsed = lrc::parse(text);
        if parsed.is_empty() {
            return Ok(LyricsOutcome::NotFound);
        }
        let synced = parsed.synced;

        let rel = format!("{track_id}.lrc");
        tokio::fs::create_dir_all(&self.lyrics_root)
            .await
            .map_err(AppError::Io)?;
        let dest = self.lyrics_root.join(&rel);
        tokio::fs::write(&dest, text.as_bytes())
            .await
            .map_err(AppError::Io)?;

        self.library
            .set_track_lyrics(
                caller,
                track_id,
                LyricsMeta {
                    lyrics_path: rel,
                    synced,
                    source: source.to_string(),
                    source_sig: sig.to_string(),
                },
            )
            .await?;

        // Embed back into the file for network/manual sources (embedded is
        // already in the file; a sidecar is left untouched by default).
        if self.write_back
            && matches!(source, "lrclib" | "manual")
            && let Some(p) = audio_path
        {
            let p = p.to_path_buf();
            let text = text.to_string();
            match tokio::task::spawn_blocking(move || tag::write_lyrics(&p, &text)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(track = %track_id, error = %e, "lyrics: tag write-back failed")
                }
                Err(e) => {
                    tracing::warn!(track = %track_id, error = %e, "lyrics: write-back task panicked")
                }
            }
        }
        Ok(LyricsOutcome::Resolved { synced })
    }

    fn resolve_path(&self, file_path: &str) -> Option<PathBuf> {
        let p = Path::new(file_path);
        if p.is_absolute() {
            Some(p.to_path_buf())
        } else {
            self.library_root.as_ref().map(|r| r.join(p))
        }
    }
}

/// Pick the winning source for a track — sidecar → embedded tag → LRCLIB —
/// **without** touching the DB (so the priority is unit-testable with just a
/// fake source + a tempdir).
async fn resolve_source(
    source: Option<&Arc<dyn LyricsSource>>,
    path: &Path,
    q: &LyricQuery<'_>,
) -> Result<Resolution> {
    // 1. Sidecar `.lrc` next to the source file (or a folder-level `lyrics.lrc`).
    if let Some(text) = local_sidecar(path) {
        return Ok(Resolution::Hit {
            text,
            source: "sidecar",
        });
    }
    // 2. Embedded lyric tag (USLT/SYLT / LYRICS / ©lyr).
    if let Ok(Some(text)) = tag::read_embedded_lyrics(path) {
        return Ok(Resolution::Hit {
            text,
            source: "embedded",
        });
    }
    // 3. External provider (only when LYRICS_FETCH is on → source is Some).
    if let Some(source) = source {
        return Ok(match source.fetch(q).await? {
            Some(LyricResult::Lyrics { text, .. }) => Resolution::Hit {
                text,
                source: "lrclib",
            },
            Some(LyricResult::Instrumental) => Resolution::Instrumental,
            None => Resolution::None,
        });
    }
    Ok(Resolution::None)
}

/// File-content signature (size + mtime) so a replaced audio file re-resolves.
fn source_sig(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{}:{}", m.len(), mtime))
}

/// Scan the source folder for a `<basename>.lrc` (then a folder-level
/// `lyrics.lrc`), like artwork's `local_cover`.
fn local_sidecar(audio_path: &Path) -> Option<String> {
    let dir = audio_path.parent()?;
    let stem = audio_path.file_stem()?.to_str()?;
    for name in [format!("{stem}.lrc"), "lyrics.lrc".to_string()] {
        if let Ok(s) = std::fs::read_to_string(dir.join(&name))
            && !s.trim().is_empty()
        {
            return Some(s);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeSource(Option<LyricResult>);
    #[async_trait]
    impl LyricsSource for FakeSource {
        async fn fetch(&self, _q: &LyricQuery<'_>) -> Result<Option<LyricResult>> {
            Ok(self.0.clone())
        }
    }

    fn q() -> LyricQuery<'static> {
        LyricQuery {
            artist: "Mara Vesper",
            title: "Halcyon Drift",
            album: "Halcyon Drift",
            duration_secs: 222,
        }
    }

    #[tokio::test]
    async fn sidecar_beats_provider() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("song.flac");
        std::fs::write(&audio, b"not really audio").unwrap();
        std::fs::write(dir.path().join("song.lrc"), "[00:01.00]from sidecar").unwrap();

        // Provider would return something else, but the sidecar must win.
        let src: Arc<dyn LyricsSource> = Arc::new(FakeSource(Some(LyricResult::Lyrics {
            text: "from network".into(),
            synced: false,
        })));
        let res = resolve_source(Some(&src), &audio, &q()).await.unwrap();
        match res {
            Resolution::Hit { text, source } => {
                assert_eq!(source, "sidecar");
                assert!(text.contains("from sidecar"));
            }
            _ => panic!("expected sidecar hit"),
        }
    }

    #[tokio::test]
    async fn folder_level_sidecar_used() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("song.flac");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(dir.path().join("lyrics.lrc"), "plain words").unwrap();
        let res = resolve_source(None, &audio, &q()).await.unwrap();
        assert!(matches!(res, Resolution::Hit { source: "sidecar", .. }));
    }

    #[tokio::test]
    async fn provider_used_when_no_local() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("song.flac");
        std::fs::write(&audio, b"x").unwrap();
        let src: Arc<dyn LyricsSource> = Arc::new(FakeSource(Some(LyricResult::Lyrics {
            text: "[00:02.00]network line".into(),
            synced: true,
        })));
        let res = resolve_source(Some(&src), &audio, &q()).await.unwrap();
        assert!(matches!(res, Resolution::Hit { source: "lrclib", .. }));
    }

    #[tokio::test]
    async fn provider_instrumental_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("song.flac");
        std::fs::write(&audio, b"x").unwrap();
        let src: Arc<dyn LyricsSource> = Arc::new(FakeSource(Some(LyricResult::Instrumental)));
        let res = resolve_source(Some(&src), &audio, &q()).await.unwrap();
        assert!(matches!(res, Resolution::Instrumental));
    }

    #[tokio::test]
    async fn no_source_no_local_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("song.flac");
        std::fs::write(&audio, b"x").unwrap();
        let res = resolve_source(None, &audio, &q()).await.unwrap();
        assert!(matches!(res, Resolution::None));
    }

    #[test]
    fn source_sig_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.flac");
        std::fs::write(&f, b"12345").unwrap();
        let a = source_sig(&f).unwrap();
        std::fs::write(&f, b"1234567890").unwrap();
        let b = source_sig(&f).unwrap();
        assert_ne!(a, b); // size differs
    }
}
