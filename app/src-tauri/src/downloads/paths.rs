//! Path resolution + sanitisation for downloaded content.
//!
//! Layout mirrors the server's `Organizer` (`Artist/Album/Track.ext`) so a
//! downloaded library browses the same way on disk as it does in-app. All
//! path components are sanitised so user-controlled metadata can't escape
//! the downloads root (no `..`, no absolute paths, no NULs).

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};

/// Settings key holding the downloads-root override (absolute path).
pub const SETTING_DOWNLOADS_DIR: &str = "downloads_dir";
/// Settings key for the mobile "Wi-Fi only" toggle (`"true"`/`"false"`).
pub const SETTING_WIFI_ONLY: &str = "wifi_only";

/// Make a single path component safe to embed. Strips path separators,
/// control chars, and the literal `..`, and trims whitespace/dots so we
/// don't end up with hidden or empty components.
pub fn sanitize(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    for ch in component.chars() {
        if ch.is_control() {
            continue;
        }
        if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue;
        }
        out.push(ch);
    }
    let trimmed = out.trim().trim_matches('.').trim();
    if trimmed.is_empty() || trimmed == ".." {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `<root>/<artist>/<album>/<name>.<ext>` — the canonical download path
/// for one media file or cover.
pub fn track_path(
    root: &Path,
    artist: &str,
    album: &str,
    file_name: &str,
    ext: &str,
) -> PathBuf {
    root.join(sanitize(artist))
        .join(sanitize(album))
        .join(format!("{}.{}", sanitize(file_name), sanitize(ext)))
}

/// Derive a sensible file extension for a track from the server's
/// `file_path` / `codec`. Falls back to `bin` when we can't tell.
pub fn track_extension(server_file_path: &str, codec: &str) -> String {
    if let Some(ext) = Path::new(server_file_path).extension().and_then(|e| e.to_str()) {
        let ext = ext.trim();
        if !ext.is_empty() {
            return ext.to_ascii_lowercase();
        }
    }
    match codec.to_ascii_lowercase().as_str() {
        "mp3" => "mp3",
        "flac" => "flac",
        "wav" => "wav",
        "ogg" | "vorbis" => "ogg",
        "opus" => "opus",
        "aac" => "m4a",
        "mp4" | "m4a" => "m4a",
        _ => "bin",
    }
    .to_string()
}

/// The temp suffix used while a download is in flight. The manager streams
/// to `<final>.part` then renames atomically on completion so a half file
/// never looks like a finished download to the player protocol.
pub const PART_SUFFIX: &str = "part";

/// Resolve a track's display name: prefer `NN - Title` (zero-padded track
/// number), falling back to just the title, then the id.
pub fn track_file_name(track_no: Option<i64>, title: &str, id: &str) -> String {
    let raw_title = title.trim();
    let title = sanitize(raw_title);
    match track_no {
        Some(n) if n > 0 => format!("{:02} - {}", n, title),
        _ => {
            if raw_title.is_empty() {
                sanitize(id)
            } else {
                title
            }
        }
    }
}

/// Ensure a directory exists, creating it (and parents) if needed.
pub async fn ensure_dir(path: &Path) -> AppResult<()> {
    tokio::fs::create_dir_all(path).await.map_err(|e| {
        AppError::Internal(format!("create dir {}: {e}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_separators_and_dots() {
        assert_eq!(sanitize("../etc"), "etc");
        assert_eq!(sanitize("a/b\\c:d"), "abcd");
        assert_eq!(sanitize("  ..  "), "unknown");
        assert_eq!(sanitize(""), "unknown");
        assert_eq!(sanitize("OK Name"), "OK Name");
    }

    #[test]
    fn extension_prefers_file_path() {
        assert_eq!(track_extension("/lib/song.flac", "mp3"), "flac");
        assert_eq!(track_extension("song", "FLAC"), "flac");
        assert_eq!(track_extension("song", "opus"), "opus");
        assert_eq!(track_extension("song", "weird"), "bin");
    }

    #[test]
    fn file_name_pads_track_no() {
        assert_eq!(track_file_name(Some(3), "Song", "id"), "03 - Song");
        assert_eq!(track_file_name(None, "Song", "id"), "Song");
        assert_eq!(track_file_name(None, "", "uuid"), "uuid");
    }
}
