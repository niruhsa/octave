//! File organisation for uploads and ingest.
//!
//! The [`Organizer`] computes an `Artist/Album/Track.ext` destination under
//! the library root and **copies** the source file there — the source is
//! **never moved or deleted** (ingest = copy only).
//!
//! Path components are sanitised and metadata fallbacks are `Unknown`.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::error::{AppError, Result};
use crate::services::tag::TagInfo;

/// Manages the `LIBRARY_PATH` root and provides copy-into-library operations.
#[derive(Clone)]
pub struct Organizer {
    pub library_root: PathBuf,
}

impl Organizer {
    pub fn new(library_root: PathBuf) -> Self {
        Self { library_root }
    }

    /// Compute the organised destination for `source` given its tags.
    ///
    /// Layout: `<library_root>/<Language>/<Artist>/<Album>/<Track>.<ext>`
    ///
    /// - `Language` is the primary artist's main language (from the file's
    ///   `Language` tag if set, otherwise script-inferred from the artist
    ///   name).
    /// - `Artist` is the **primary artist only** — `feat.`/`&`/`,` collabs
    ///   are stripped upstream in [`crate::services::tag::primary_artist`]
    ///   so one artist's catalog stays in one folder.
    ///
    /// Missing metadata falls back to `Unknown`.  Track numbers are
    /// zero-padded to two digits (`01`, `02`, …).
    pub fn compute_destination(&self, source: &Path, tags: &TagInfo) -> PathBuf {
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");

        let track_name = match tags.track_no {
            Some(n) => format!("{:02} - {}", n, sanitize(&tags.title)),
            None => sanitize(&tags.title),
        };

        self.library_root
            .join(sanitize(&tags.language))
            .join(sanitize(&tags.artist))
            .join(sanitize(&tags.album))
            .join(format!("{track_name}.{ext}"))
    }

    /// Copy `source` to its organised destination.
    ///
    /// Parent directories are created automatically.  If the destination
    /// already exists and has the same size the copy is skipped (idempotent).
    ///
    /// Returns the absolute destination path.
    pub fn organize(&self, source: &Path, tags: &TagInfo) -> Result<PathBuf> {
        let dest = self.compute_destination(source, tags);

        if dest.exists() {
            let src_len = std::fs::metadata(source)
                .map(|m| m.len())
                .unwrap_or(0);
            let dst_len = std::fs::metadata(&dest)
                .map(|m| m.len())
                .unwrap_or(u64::MAX);
            if src_len == dst_len {
                debug!(dest = %dest.display(), "organize: destination exists with same size, skipping copy");
                return Ok(dest);
            }
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::Internal(format!(
                    "create dir {}: {e}",
                    parent.display()
                ))
            })?;
        }
        std::fs::copy(source, &dest).map_err(|e| {
            AppError::Internal(format!(
                "copy {} -> {}: {e}",
                source.display(),
                dest.display()
            ))
        })?;
        debug!(src = %source.display(), dest = %dest.display(), "organize: copied");
        Ok(dest)
    }
}

/// Sanitise a single path component.
///
/// Characters that are problematic on common filesystems (`/`, `\`, `:`, `*`,
/// `?`, `"`, `<`, `>`, `|`, NUL) are replaced with `_`.  Leading/trailing
/// dots, whitespace, and underscores are stripped so that inputs like `"/"`
/// or `"..."` correctly fall through to the `"Unknown"` default.
pub fn sanitize(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c => c,
        })
        .collect();
    out = out
        .trim()
        .trim_matches(|c: char| c == '.' || c == '_')
        .to_string();
    if out.is_empty() {
        "Unknown".to_string()
    } else {
        out
    }
}

/// Returns `true` when `path` lives inside `root` (after canonicalising both).
pub fn is_under(path: &Path, root: &Path) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(p), Ok(r)) => p.starts_with(&r),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_passthrough() {
        assert_eq!(sanitize("Pink Floyd"), "Pink Floyd");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(
            sanitize("AC/DC: Back in Black"),
            "AC_DC_ Back in Black"
        );
    }

    #[test]
    fn sanitize_empty_fallback() {
        assert_eq!(sanitize(""), "Unknown");
        assert_eq!(sanitize("..."), "Unknown");
        assert_eq!(sanitize(" / "), "Unknown");
        assert_eq!(sanitize("/"), "Unknown");
        assert_eq!(sanitize("___"), "Unknown");
    }

    fn mk_tags(
        title: &str,
        artist: &str,
        album: &str,
        language: &str,
        track_no: Option<i32>,
    ) -> TagInfo {
        TagInfo {
            title: title.into(),
            artist: artist.into(),
            artist_raw: artist.into(),
            album: album.into(),
            language: language.into(),
            track_no,
            disc_no: None,
            year: None,
            duration_ms: 0,
            bitrate_kbps: None,
            codec: "flac".into(),
            file_size: None,
        }
    }

    #[test]
    fn compute_destination_basic() {
        let org = Organizer::new(PathBuf::from("/music"));
        let tags = mk_tags(
            "Money",
            "Pink Floyd",
            "Dark Side of the Moon",
            "English",
            Some(6),
        );
        let dest = org.compute_destination(Path::new("input.flac"), &tags);
        assert_eq!(
            dest,
            PathBuf::from("/music/English/Pink Floyd/Dark Side of the Moon/06 - Money.flac")
        );
    }

    #[test]
    fn compute_destination_japanese_artist() {
        let org = Organizer::new(PathBuf::from("/music"));
        let tags = mk_tags("First Love", "宇多田ヒカル", "First Love", "Japanese", Some(1));
        let dest = org.compute_destination(Path::new("input.flac"), &tags);
        assert_eq!(
            dest,
            PathBuf::from("/music/Japanese/宇多田ヒカル/First Love/01 - First Love.flac")
        );
    }

    #[test]
    fn compute_destination_unknown_metadata() {
        let org = Organizer::new(PathBuf::from("/music"));
        let tags = mk_tags("", "", "", "", None);
        let dest = org.compute_destination(Path::new("mystery.mp3"), &tags);
        assert_eq!(
            dest,
            PathBuf::from("/music/Unknown/Unknown/Unknown/Unknown.mp3")
        );
    }
}
