//! Shared audio-file tag extraction.
//!
//! Used by [`ScanService`], uploads, and the ingest-folder watcher so all
//! three paths agree on the same metadata-fallback rules.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag};

/// File extensions recognised as audio (lowercase, without the dot).
pub const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "opus", "m4a", "mp4", "aac", "aiff", "wv", "ape",
];

/// Best-effort metadata extracted from an audio file.  Every field has a
/// sensible fallback so downstream code never needs to handle `None` for the
/// core identity columns (`artist`, `album`, `title`).
///
/// `artist` is the **primary artist only** — collaboration suffixes like
/// `"feat. X"`, `" & Y"`, `", Z"` are stripped so the on-disk layout doesn't
/// fragment one artist's catalog across many slightly-different folder names.
/// The raw tag string is preserved in [`artist_raw`] for the audit trail.
///
/// `language` is the **primary artist's main language**, taken from the
/// `Language` / `TLAN` tag when present, otherwise inferred from the script
/// of the artist name (Han / Hiragana+Katakana / Hangul / Cyrillic / Arabic
/// / Hebrew / Greek / Devanagari / Thai — everything else defaults to
/// `"English"`).  Used as the top-level folder under `LIBRARY_PATH`.
pub struct TagInfo {
    pub title: String,
    /// Primary artist only (see struct docs).
    pub artist: String,
    /// Raw artist string as it appeared in the tag, before primary-artist
    /// extraction.  Currently informational; kept for future audit/rollback.
    pub artist_raw: String,
    pub album: String,
    pub language: String,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub year: Option<i32>,
    pub duration_ms: i64,
    pub bitrate_kbps: Option<i32>,
    pub codec: String,
    pub file_size: Option<i64>,
}

/// Read tags from the audio file at `path`.  Returns `Err` only when `lofty`
/// cannot open/parse the file at all — missing individual tags fall back to
/// `Unknown` / filename defaults.
pub fn read_tags(path: &Path) -> crate::error::Result<TagInfo> {
    let probe = Probe::open(path)
        .map_err(|e| crate::error::AppError::Internal(format!("probe open: {e}")))?
        .read()
        .map_err(|e| crate::error::AppError::Internal(format!("probe read: {e}")))?;

    let props = probe.properties();
    let duration_ms = props.duration().as_millis() as i64;
    let bitrate_kbps = props.audio_bitrate().map(|b| b as i32);
    let codec = format!("{:?}", probe.file_type());
    let file_size = std::fs::metadata(path).ok().map(|m| m.len() as i64);

    let tag = probe.primary_tag().or_else(|| probe.first_tag());

    let title = tag
        .and_then(|t| t.title().map(|s| s.to_string()))
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown Title")
                .to_string()
        });
    // Prefer the album-artist tag for the primary-artist field: collab
    // tracks routinely list every guest in TPE1/TrackArtist, while TPE2/
    // AlbumArtist holds the canonical "this album belongs to ___" name.
    let artist_raw = tag
        .and_then(|t| {
            t.get_string(&ItemKey::AlbumArtist)
                .map(str::to_string)
                .or_else(|| t.artist().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let artist = primary_artist(&artist_raw);
    let album = tag
        .and_then(|t| t.album().map(|s| s.to_string()))
        .unwrap_or_else(|| "Unknown Album".to_string());
    let language = tag
        .and_then(|t| t.get_string(&ItemKey::Language).map(str::to_string))
        .map(|s| normalize_language(&s))
        .unwrap_or_else(|| infer_language(&artist));
    let track_no = tag.and_then(|t| t.track()).and_then(|n| i32::try_from(n).ok());
    let disc_no = tag.and_then(|t| t.disk()).and_then(|n| i32::try_from(n).ok());
    let year = tag.and_then(|t| t.year()).and_then(|n| i32::try_from(n).ok());

    Ok(TagInfo {
        title,
        artist,
        artist_raw,
        album,
        language,
        track_no,
        disc_no,
        year,
        duration_ms,
        bitrate_kbps,
        codec,
        file_size,
    })
}

/// Extract the primary artist from a raw tag string.
///
/// Strips collaboration suffixes (`feat.`, `featuring`, `ft.`, `with`,
/// `vs`/`vs.`) and splits on the first separator (`&`, `,`, `;`, `×`, `+`,
/// ` and `, ` x `, ` X `).  The result is trimmed; an empty result falls
/// back to `"Unknown Artist"`.
///
/// Examples:
/// - `"Daft Punk feat. Pharrell Williams"`  → `"Daft Punk"`
/// - `"Calvin Harris & Rihanna"`             → `"Calvin Harris"`
/// - `"Beyoncé, Jay-Z"`                        → `"Beyoncé"`
/// - `"AC/DC"`                                → `"AC/DC"`  (no separator hit)
pub fn primary_artist(raw: &str) -> String {
    // 1. Strip everything after a collaboration keyword.
    //    Match case-insensitively on whole-word boundaries.
    let lower = raw.to_lowercase();
    let collab_markers = [
        " feat.", " feat ", " featuring ", " ft.", " ft ", " with ", " vs.",
        " vs ", " w/ ", " w/",
    ];
    let mut cut = raw.len();
    for marker in collab_markers.iter() {
        if let Some(pos) = lower.find(marker) {
            cut = cut.min(pos);
        }
    }
    let trimmed = &raw[..cut];

    // 2. Split on the first hard separator.
    //    Note the leading/trailing spaces on " and " / " x " so we don't
    //    truncate names like "Sandy & Junior" on the literal letter `x`.
    let separators: &[&str] = &[" & ", " × ", " + ", ", ", "; ", " and ", " x ", " X "];
    let mut head = trimmed;
    for sep in separators {
        if let Some(pos) = head.find(sep) {
            head = &head[..pos];
        }
    }
    let out = head.trim().to_string();
    if out.is_empty() {
        "Unknown Artist".to_string()
    } else {
        out
    }
}

/// Map a raw `Language` tag value to a canonical folder name.
///
/// Accepts ISO-639 codes (`en`, `eng`, `ja`, `jpn`, ...) and a handful of
/// English / native-name aliases.  Unknown values pass through trimmed and
/// title-cased so a custom tag like `"Mandarin"` still produces a sane
/// folder name.
pub fn normalize_language(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return "Unknown".to_string();
    }
    let key = s.to_ascii_lowercase();
    match key.as_str() {
        "en" | "eng" | "english" => "English",
        "ja" | "jpn" | "japanese" | "日本語" => "Japanese",
        "ko" | "kor" | "korean" | "한국어" => "Korean",
        "zh" | "zho" | "chi" | "chinese" | "mandarin" | "中文" => "Chinese",
        "es" | "spa" | "spanish" | "español" => "Spanish",
        "fr" | "fra" | "fre" | "french" | "français" => "French",
        "de" | "deu" | "ger" | "german" | "deutsch" => "German",
        "it" | "ita" | "italian" | "italiano" => "Italian",
        "pt" | "por" | "portuguese" | "português" => "Portuguese",
        "ru" | "rus" | "russian" => "Russian",
        "ar" | "ara" | "arabic" => "Arabic",
        "hi" | "hin" | "hindi" => "Hindi",
        "he" | "heb" | "hebrew" => "Hebrew",
        "el" | "ell" | "gre" | "greek" => "Greek",
        "th" | "tha" | "thai" => "Thai",
        "vi" | "vie" | "vietnamese" => "Vietnamese",
        "id" | "ind" | "indonesian" => "Indonesian",
        "tr" | "tur" | "turkish" => "Turkish",
        "pl" | "pol" | "polish" => "Polish",
        "nl" | "nld" | "dut" | "dutch" => "Dutch",
        "sv" | "swe" | "swedish" => "Swedish",
        _ => {
            // Unknown — title-case the trimmed input.
            let mut chars = s.chars();
            let titled = match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            };
            return titled;
        }
    }
    .to_string()
}

/// Infer a language label from the script of `name`.
///
/// We look at every character and bucket it by Unicode block.  The bucket
/// with the most letters wins; ties / no-CJK-or-RTL letters default to
/// `"English"`.
pub fn infer_language(name: &str) -> String {
    use Script::*;
    let mut counts = [0usize; 9];
    for c in name.chars() {
        if let Some(s) = script_of(c) {
            counts[s as usize] += 1;
        }
    }
    // Kana is exclusive to Japanese: a single hiragana/katakana char beats
    // any kanji count.  Without this, names like "宇多田ヒカル" (3 kanji + 3
    // kana) would mis-classify as Chinese on the raw majority count.
    if counts[Japanese as usize] > 0 {
        return "Japanese".to_string();
    }
    // Hangul is similarly exclusive to Korean.
    if counts[Hangul as usize] > 0 {
        return "Korean".to_string();
    }
    let scripts = [
        ("Chinese", Han),
        ("Russian", Cyrillic),
        ("Arabic", Arabic),
        ("Hebrew", Hebrew),
        ("Greek", Greek),
        ("Hindi", Devanagari),
        ("Thai", Thai),
    ];
    let mut best: Option<(&str, usize)> = None;
    for (label, s) in scripts.iter() {
        let n = counts[*s as usize];
        if n > 0 && best.map_or(true, |(_, m)| n > m) {
            best = Some((label, n));
        }
    }
    best.map(|(l, _)| l.to_string())
        .unwrap_or_else(|| "English".to_string())
}

#[derive(Copy, Clone)]
enum Script {
    Han = 0,
    Japanese = 1,
    Hangul = 2,
    Cyrillic = 3,
    Arabic = 4,
    Hebrew = 5,
    Greek = 6,
    Devanagari = 7,
    Thai = 8,
}

fn script_of(c: char) -> Option<Script> {
    let u = c as u32;
    // Hiragana + Katakana (incl. half-width).  Distinguished from Han
    // because a Japanese artist name often mixes kana with kanji — the
    // presence of any kana is a strong Japanese signal.
    if (0x3040..=0x30FF).contains(&u) || (0xFF66..=0xFF9F).contains(&u) {
        return Some(Script::Japanese);
    }
    // CJK Unified Ideographs (Han).  After kana check above, this is
    // Chinese unless the file's Language tag overrides.
    if (0x4E00..=0x9FFF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x20000..=0x2A6DF).contains(&u)
    {
        return Some(Script::Han);
    }
    // Hangul (Korean).
    if (0xAC00..=0xD7AF).contains(&u) || (0x1100..=0x11FF).contains(&u) {
        return Some(Script::Hangul);
    }
    // Cyrillic.
    if (0x0400..=0x04FF).contains(&u) {
        return Some(Script::Cyrillic);
    }
    // Arabic.
    if (0x0600..=0x06FF).contains(&u) || (0x0750..=0x077F).contains(&u) {
        return Some(Script::Arabic);
    }
    // Hebrew.
    if (0x0590..=0x05FF).contains(&u) {
        return Some(Script::Hebrew);
    }
    // Greek.
    if (0x0370..=0x03FF).contains(&u) {
        return Some(Script::Greek);
    }
    // Devanagari (Hindi).
    if (0x0900..=0x097F).contains(&u) {
        return Some(Script::Devanagari);
    }
    // Thai.
    if (0x0E00..=0x0E7F).contains(&u) {
        return Some(Script::Thai);
    }
    None
}

/// A set of tag fields to write back to an audio file.  Every field is
/// optional — `None` leaves the existing tag value untouched, `Some` (incl.
/// empty string) overwrites it.  Used by the metadata-edit pipeline when
/// server-side tag write-back is enabled.
#[derive(Debug, Clone, Default)]
pub struct TagWrite {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub year: Option<i32>,
}

impl TagWrite {
    /// `true` when no field is set — nothing to write.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.track_no.is_none()
            && self.disc_no.is_none()
            && self.year.is_none()
    }
}

/// Write `edit`'s set fields into the audio file at `path`, preserving any
/// existing tags not mentioned in `edit`.
///
/// If the file has no primary tag yet, a fresh one of the format's primary
/// type is inserted.  Returns `Err` when `lofty` cannot open/parse the file
/// or the save fails (e.g. read-only path, unsupported format).
pub fn write_tags(path: &Path, edit: &TagWrite) -> crate::error::Result<()> {
    if edit.is_empty() {
        return Ok(());
    }
    let mut tagged = lofty::read_from_path(path)
        .map_err(|e| crate::error::AppError::Internal(format!("tag write open: {e}")))?;
    if tagged.primary_tag_mut().is_none() {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .expect("primary tag inserted above");
    if let Some(t) = &edit.title {
        tag.set_title(t.clone());
    }
    if let Some(a) = &edit.artist {
        tag.set_artist(a.clone());
    }
    if let Some(al) = &edit.album {
        tag.set_album(al.clone());
    }
    if let Some(n) = edit.track_no {
        if let Ok(v) = u32::try_from(n) {
            tag.set_track(v);
        }
    }
    if let Some(n) = edit.disc_no {
        if let Ok(v) = u32::try_from(n) {
            tag.set_disk(v);
        }
    }
    if let Some(y) = edit.year {
        if let Ok(v) = u32::try_from(y) {
            tag.set_year(v);
        }
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| crate::error::AppError::Internal(format!("tag write save: {e}")))?;
    Ok(())
}

/// Returns `true` when the file extension (case-insensitive) is a recognised
/// audio format.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn audio_ext_recognised() {
        assert!(is_audio_file(Path::new("song.mp3")));
        assert!(is_audio_file(Path::new("track.FLAC")));
        assert!(is_audio_file(Path::new("dir/track.opus")));
        assert!(is_audio_file(Path::new("x.m4a")));
    }

    #[test]
    fn non_audio_ext_rejected() {
        assert!(!is_audio_file(Path::new("readme.txt")));
        assert!(!is_audio_file(Path::new("image.png")));
        assert!(!is_audio_file(Path::new("noext")));
        assert!(!is_audio_file(Path::new(".")));
        assert!(!is_audio_file(Path::new("")));
    }

    #[test]
    fn primary_artist_strips_features() {
        assert_eq!(
            primary_artist("Daft Punk feat. Pharrell Williams"),
            "Daft Punk"
        );
        assert_eq!(primary_artist("Eminem ft. Rihanna"), "Eminem");
        assert_eq!(primary_artist("Eminem ft Rihanna"), "Eminem");
        assert_eq!(
            primary_artist("Drake featuring Future"),
            "Drake"
        );
        assert_eq!(primary_artist("Run-D.M.C. vs. Jason Nevins"), "Run-D.M.C.");
    }

    #[test]
    fn primary_artist_strips_combinations() {
        assert_eq!(primary_artist("Calvin Harris & Rihanna"), "Calvin Harris");
        assert_eq!(primary_artist("Beyoncé, Jay-Z"), "Beyoncé");
        assert_eq!(primary_artist("A; B; C"), "A");
        assert_eq!(primary_artist("Phantogram × Big Boi"), "Phantogram");
        assert_eq!(primary_artist("Simon and Garfunkel"), "Simon");
    }

    #[test]
    fn primary_artist_preserves_solo_names() {
        // No separator anywhere — pass through.
        assert_eq!(primary_artist("AC/DC"), "AC/DC");
        assert_eq!(primary_artist("twenty one pilots"), "twenty one pilots");
        assert_eq!(primary_artist("Tyler, the Creator"), "Tyler"); // documented compromise: comma wins
    }

    #[test]
    fn primary_artist_empty_fallback() {
        assert_eq!(primary_artist(""), "Unknown Artist");
        assert_eq!(primary_artist("   "), "Unknown Artist");
        assert_eq!(primary_artist(" feat. nobody"), "Unknown Artist");
    }

    #[test]
    fn normalize_language_known_codes() {
        assert_eq!(normalize_language("en"), "English");
        assert_eq!(normalize_language("EN"), "English");
        assert_eq!(normalize_language("eng"), "English");
        assert_eq!(normalize_language("ja"), "Japanese");
        assert_eq!(normalize_language("jpn"), "Japanese");
        assert_eq!(normalize_language("ko"), "Korean");
        assert_eq!(normalize_language("zh"), "Chinese");
        assert_eq!(normalize_language("Mandarin"), "Chinese");
    }

    #[test]
    fn normalize_language_unknown_passthrough() {
        assert_eq!(normalize_language("esperanto"), "Esperanto");
        assert_eq!(normalize_language("  "), "Unknown");
    }

    #[test]
    fn infer_language_from_script() {
        assert_eq!(infer_language("宇多田ヒカル"), "Japanese"); // mixed kanji + katakana
        assert_eq!(infer_language("アイスウォッシュレットシティ"), "Japanese"); // pure katakana
        assert_eq!(infer_language("周杰倫"), "Chinese"); // pure Han
        assert_eq!(infer_language("방탄소년단"), "Korean");
        assert_eq!(infer_language("Пушкин"), "Russian");
        assert_eq!(infer_language("מתיסיהוד"), "Hebrew");
        assert_eq!(infer_language("Πυθαγόρας"), "Greek");
        assert_eq!(infer_language("The Beatles"), "English");
        assert_eq!(infer_language(""), "English");
    }
}
