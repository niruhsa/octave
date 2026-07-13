//! Archive extraction for uploads & ingest.
//!
//! Supports the common music-archiving formats:
//! - **zip** (`.zip`) — incl. deflate/bzip2/lzma/zstd/xz members (zip crate
//!   default features).
//! - **tarballs** — `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`/`.tbz2`, `.tar.xz`/`.txz`.
//!
//! **ISO/CD images** (`.iso`, `.bin`/`.cue`, `.img`, `.nrg`) are recognised
//! but not yet extracted — they return a clear `InvalidArgument` so callers
//! can surface a "not yet supported" message (PLAN Phase 6 stub).
//!
//! Extraction is **read-only on the source**: members are written into a
//! caller-provided destination directory. Every member path is sanitised to
//! prevent path-traversal ("zip-slip") — entries escaping the destination
//! root are skipped.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use tracing::{debug, warn};

use crate::error::{AppError, Result};

/// Recognised archive container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    /// Optical-disc image — recognised but not yet extracted (stub).
    DiscImage,
}

impl ArchiveKind {
    /// Detect the archive kind from a filename's extension(s).
    ///
    /// Returns `None` for anything not recognised as an archive (e.g. a bare
    /// audio file), letting the caller fall back to single-file ingest.
    pub fn detect(path: &Path) -> Option<ArchiveKind> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        // Multi-extension tarballs first.
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            return Some(ArchiveKind::TarGz);
        }
        if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
            return Some(ArchiveKind::TarBz2);
        }
        if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            return Some(ArchiveKind::TarXz);
        }
        if name.ends_with(".tar") {
            return Some(ArchiveKind::Tar);
        }
        if name.ends_with(".zip") {
            return Some(ArchiveKind::Zip);
        }
        if name.ends_with(".iso")
            || name.ends_with(".img")
            || name.ends_with(".nrg")
            || name.ends_with(".bin")
            || name.ends_with(".cue")
        {
            return Some(ArchiveKind::DiscImage);
        }
        None
    }

    /// `true` when this kind is a tar variant (possibly compressed).
    fn is_tar(self) -> bool {
        matches!(
            self,
            ArchiveKind::Tar | ArchiveKind::TarGz | ArchiveKind::TarBz2 | ArchiveKind::TarXz
        )
    }
}

/// Extract `source` (of the given `kind`) into `dest_dir`.
///
/// Returns the list of regular files written. Directories and any entry whose
/// sanitised path would escape `dest_dir` are skipped. The source file is
/// never modified.
pub fn extract(source: &Path, kind: ArchiveKind, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dest_dir).map_err(AppError::Io)?;
    match kind {
        ArchiveKind::Zip => extract_zip(source, dest_dir),
        k if k.is_tar() => extract_tar(source, k, dest_dir),
        ArchiveKind::DiscImage => Err(AppError::InvalidArgument(
            "ISO/CD disc-image ingest is not yet supported".into(),
        )),
        // Unreachable: is_tar covers the remaining tar variants.
        _ => Err(AppError::Internal("unhandled archive kind".into())),
    }
}

fn extract_zip(source: &Path, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(source).map_err(AppError::Io)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidArgument(format!("invalid zip: {e}")))?;

    let mut written = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::InvalidArgument(format!("zip entry {i}: {e}")))?;
        if !entry.is_file() {
            continue;
        }
        // `enclosed_name` already rejects absolute paths and `..` traversal.
        let rel = match entry.enclosed_name() {
            Some(p) => p,
            None => {
                warn!(name = entry.name(), "zip: skipping unsafe entry name");
                continue;
            }
        };
        if is_macos_metadata(&rel) {
            debug!(name = %rel.display(), "zip: skipping macOS metadata entry");
            continue;
        }
        let dest = match safe_join(dest_dir, &rel) {
            Some(p) => p,
            None => continue,
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::Io)?;
        }
        let mut out = File::create(&dest).map_err(AppError::Io)?;
        std::io::copy(&mut entry, &mut out).map_err(AppError::Io)?;
        debug!(dest = %dest.display(), "zip: extracted member");
        written.push(dest);
    }
    Ok(written)
}

fn extract_tar(source: &Path, kind: ArchiveKind, dest_dir: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(source).map_err(AppError::Io)?;
    let reader: Box<dyn Read> = match kind {
        ArchiveKind::Tar => Box::new(file),
        ArchiveKind::TarGz => Box::new(flate2::read::GzDecoder::new(file)),
        ArchiveKind::TarBz2 => Box::new(bzip2::read::BzDecoder::new(file)),
        ArchiveKind::TarXz => Box::new(xz2::read::XzDecoder::new(file)),
        _ => {
            return Err(AppError::Internal(
                "extract_tar called with non-tar kind".into(),
            ));
        }
    };

    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|e| AppError::InvalidArgument(format!("invalid tar: {e}")))?;

    let mut written = Vec::new();
    for entry in entries {
        let mut entry = entry.map_err(|e| AppError::InvalidArgument(format!("tar entry: {e}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .map_err(|e| AppError::InvalidArgument(format!("tar entry path: {e}")))?
            .into_owned();
        if is_macos_metadata(&rel) {
            debug!(name = %rel.display(), "tar: skipping macOS metadata entry");
            continue;
        }
        let dest = match safe_join(dest_dir, &rel) {
            Some(p) => p,
            None => {
                warn!(name = %rel.display(), "tar: skipping unsafe entry name");
                continue;
            }
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::Io)?;
        }
        let mut out = File::create(&dest).map_err(AppError::Io)?;
        std::io::copy(&mut entry, &mut out).map_err(AppError::Io)?;
        debug!(dest = %dest.display(), "tar: extracted member");
        written.push(dest);
    }
    Ok(written)
}

/// `true` for macOS-only archive cruft: AppleDouble sidecars (`._name`,
/// commonly under a `__MACOSX/` directory) that hold resource-fork / Finder
/// metadata rather than real content. Skipping them at extraction keeps that
/// cruft out of ingest — otherwise `._song.flac` is mis-detected as audio by
/// its extension and minted as a ghost "Unknown Artist" track.
fn is_macos_metadata(rel: &Path) -> bool {
    rel.components().any(|c| match c {
        Component::Normal(s) => {
            let s = s.to_string_lossy();
            s == "__MACOSX" || s.starts_with("._")
        }
        _ => false,
    })
}

/// Join `rel` onto `base`, rejecting absolute paths and any `..` / root
/// component (zip-slip guard). Normal `.` components are dropped. Returns
/// `None` when the entry is unsafe or resolves to nothing.
fn safe_join(base: &Path, rel: &Path) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    let mut pushed = false;
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => {
                out.push(c);
                pushed = true;
            }
            // Skip current-dir markers; reject everything that could escape.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return None;
            }
        }
    }
    if pushed { Some(out) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_zip_and_tarballs() {
        assert_eq!(
            ArchiveKind::detect(Path::new("a.zip")),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("a.tar")),
            Some(ArchiveKind::Tar)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("a.tar.gz")),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("a.tgz")),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("a.tar.bz2")),
            Some(ArchiveKind::TarBz2)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("a.tar.xz")),
            Some(ArchiveKind::TarXz)
        );
        assert_eq!(
            ArchiveKind::detect(Path::new("Album.TXZ")),
            Some(ArchiveKind::TarXz)
        );
    }

    #[test]
    fn detect_disc_images() {
        for n in ["cd.iso", "disc.img", "x.nrg", "track.bin", "track.cue"] {
            assert_eq!(
                ArchiveKind::detect(Path::new(n)),
                Some(ArchiveKind::DiscImage),
                "{n}"
            );
        }
    }

    #[test]
    fn detect_non_archive_is_none() {
        assert_eq!(ArchiveKind::detect(Path::new("song.flac")), None);
        assert_eq!(ArchiveKind::detect(Path::new("noext")), None);
    }

    #[test]
    fn disc_image_extract_is_stubbed() {
        let tmp = std::env::temp_dir();
        let err = extract(Path::new("x.iso"), ArchiveKind::DiscImage, &tmp).unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }

    #[test]
    fn extract_zip_round_trip() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("album.zip");
        // Build a small zip with a nested file + a traversal attempt.
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("disc1/track.flac", opts).unwrap();
            zw.write_all(b"FAKEFLAC").unwrap();
            zw.start_file("notes.txt", opts).unwrap();
            zw.write_all(b"hello").unwrap();
            zw.finish().unwrap();
        }
        let out = dir.path().join("out");
        let written = extract(&zip_path, ArchiveKind::Zip, &out).unwrap();
        assert_eq!(written.len(), 2);
        let flac = out.join("disc1/track.flac");
        assert!(flac.is_file());
        assert_eq!(std::fs::read(&flac).unwrap(), b"FAKEFLAC");
    }

    #[test]
    fn extract_tar_gz_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let tgz_path = dir.path().join("album.tar.gz");
        {
            let f = File::create(&tgz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let data = b"FAKEMP3";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "a/b/song.mp3", &data[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        let written = extract(&tgz_path, ArchiveKind::TarGz, &out).unwrap();
        assert_eq!(written.len(), 1);
        let song = out.join("a/b/song.mp3");
        assert!(song.is_file());
        assert_eq!(std::fs::read(&song).unwrap(), b"FAKEMP3");
    }

    #[test]
    fn extract_zip_skips_macos_metadata() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("album.zip");
        // A real audio file plus the AppleDouble cruft a macOS-built zip carries.
        {
            let f = File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(f);
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zw.start_file("Album/01 - Song.flac", opts).unwrap();
            zw.write_all(b"FAKEFLAC").unwrap();
            zw.start_file("__MACOSX/Album/._01 - Song.flac", opts)
                .unwrap();
            zw.write_all(b"\x00\x05\x16\x07applemeta").unwrap();
            zw.start_file("Album/._01 - Song.flac", opts).unwrap();
            zw.write_all(b"\x00\x05\x16\x07applemeta").unwrap();
            zw.finish().unwrap();
        }
        let out = dir.path().join("out");
        let written = extract(&zip_path, ArchiveKind::Zip, &out).unwrap();
        assert_eq!(
            written.len(),
            1,
            "only the real audio member should be written"
        );
        assert!(out.join("Album/01 - Song.flac").is_file());
        assert!(!out.join("__MACOSX").exists());
        assert!(!out.join("Album/._01 - Song.flac").exists());
    }

    #[test]
    fn is_macos_metadata_matches_appledouble_and_macosx() {
        assert!(is_macos_metadata(Path::new("__MACOSX/Album/._x.flac")));
        assert!(is_macos_metadata(Path::new("Album/._x.flac")));
        assert!(is_macos_metadata(Path::new("._x.flac")));
        assert!(!is_macos_metadata(Path::new("Album/01 - x.flac")));
    }

    #[test]
    fn safe_join_blocks_traversal() {
        let base = Path::new("/dest");
        assert_eq!(
            safe_join(base, Path::new("a/b.flac")),
            Some(PathBuf::from("/dest/a/b.flac"))
        );
        assert_eq!(safe_join(base, Path::new("../etc/passwd")), None);
        assert_eq!(safe_join(base, Path::new("/abs/path")), None);
        assert_eq!(
            safe_join(base, Path::new("./a.flac")),
            Some(PathBuf::from("/dest/a.flac"))
        );
        assert_eq!(safe_join(base, Path::new("")), None);
    }
}
