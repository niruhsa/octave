//! Synced + plain lyrics (Phase 15).
//!
//! Three sources, in priority order, all producing an `.lrc`/plain text blob:
//! a **`.lrc` sidecar** next to the source file, an **embedded lyric tag**
//! (read via `lofty`), and **[LRCLIB](https://lrclib.net)** (by artist / title
//! / album / duration). The blob is cached on disk under `LYRICS_PATH` and the
//! pointer + provenance recorded on the `tracks` row through [`LibraryService`]
//! (audited), exactly like album artwork under `ARTWORK_PATH`.
//!
//! Gated entirely behind `LYRICS_ENABLED`; external fetching is separately
//! gated by `LYRICS_FETCH` so an air-gapped library still uses sidecar +
//! embedded lyrics with no outbound calls.
//!
//! [`LibraryService`]: crate::services::library::LibraryService

pub mod lrc;
mod service;
mod source;

pub use lrc::{LyricLine, ParsedLyrics};
pub use service::{LyricsOutcome, LyricsReport, LyricsService, LyricsStatus, LyricsView};
pub use source::{LrcLibSource, LyricQuery, LyricResult, LyricsSource};
