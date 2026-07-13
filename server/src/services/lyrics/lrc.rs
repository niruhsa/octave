//! `.lrc` lyric parsing.
//!
//! Turns a stored `.lrc` (or plain-text) blob into structured lines the
//! client can render + seek against.  The server parses so every client
//! (desktop, Android, the eventual web client) agrees on the same timing +
//! sort order without re-implementing the format.
//!
//! Supported:
//! - `[mm:ss.xx]` / `[mm:ss.xxx]` / `[mm:ss]` line timestamps, incl. a line
//!   carrying **multiple** stamps (the text is emitted once per stamp).
//! - The `[offset:±ms]` header — a global shift applied to every stamp
//!   (`+` shifts the lyrics *earlier*, the widely-used convention).
//! - ID3-style metadata headers (`[ar:]`, `[ti:]`, `[al:]`, `[by:]`,
//!   `[length:]`, `[re:]`, `[ve:]`, `[la:]`, …) are recognised and skipped.
//! - Enhanced word-level LRC (`<mm:ss.xx>` inline tags) is parsed **down to
//!   line level** for now (the inline word stamps are stripped from the text);
//!   karaoke-style word highlighting is a clean follow-up on the same blob.

/// One lyric line: `ms` from the start of the track, and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    /// Milliseconds from the start of the track. `0` for plain (unsynced) text.
    pub ms: u32,
    pub text: String,
}

/// A parsed lyric blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLyrics {
    /// `true` when at least one line carried a timestamp.
    pub synced: bool,
    /// Lines sorted by `ms`. For plain lyrics every `ms` is `0`.
    pub lines: Vec<LyricLine>,
    /// Newline-joined text — fallback rendering + search.
    pub plain: String,
}

impl ParsedLyrics {
    /// `true` when there is no renderable text at all.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.plain.trim().is_empty()
    }
}

/// Parse `.lrc`/plain text into structured, sorted lines.
///
/// Never fails: unrecognised input degrades to plain lyrics (every non-empty
/// line at `ms = 0`, `synced = false`).
pub fn parse(raw: &str) -> ParsedLyrics {
    // Raw (pre-offset) timestamped lines, plus the plain text in file order.
    let mut timed: Vec<(i64, String)> = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();
    let mut offset_ms: i64 = 0;
    let mut any_timed = false;

    for line in raw.lines() {
        let mut rest = line.trim_start();
        let mut stamps: Vec<i64> = Vec::new();
        let mut was_meta = false;

        // Consume leading `[...]` tags (timestamps and/or metadata).
        loop {
            if !rest.starts_with('[') {
                break;
            }
            let Some(close) = rest.find(']') else { break };
            let inner = &rest[1..close];
            if let Some(ms) = parse_timestamp(inner) {
                stamps.push(ms);
                rest = rest[close + 1..].trim_start();
            } else if let Some((key, val)) = split_meta(inner) {
                if key.eq_ignore_ascii_case("offset")
                    && let Ok(v) = val.trim().parse::<i64>()
                {
                    offset_ms = v;
                }
                was_meta = true;
                rest = rest[close + 1..].trim_start();
            } else {
                // Not a tag we understand (e.g. a lyric that opens with a
                // bracket) — treat the rest of the line as text.
                break;
            }
        }

        let text = strip_word_tags(rest).trim().to_string();

        if !stamps.is_empty() {
            any_timed = true;
            for s in &stamps {
                timed.push((*s, text.clone()));
            }
            if !text.is_empty() {
                plain_lines.push(text.clone());
            }
        } else if was_meta {
            // Metadata-only line — nothing to render.
        } else if !text.is_empty() {
            plain_lines.push(text);
        }
    }

    let plain = plain_lines.join("\n");

    if any_timed {
        let mut lines: Vec<LyricLine> = timed
            .into_iter()
            .map(|(raw_ms, text)| LyricLine {
                ms: (raw_ms - offset_ms).max(0) as u32,
                text,
            })
            .collect();
        // Stable sort keeps the multi-stamp emission order for equal ms.
        lines.sort_by_key(|l| l.ms);
        ParsedLyrics {
            synced: true,
            lines,
            plain,
        }
    } else {
        let lines = plain_lines_to_lines(&plain);
        ParsedLyrics {
            synced: false,
            lines,
            plain,
        }
    }
}

/// Whether a stored blob is time-synced — a cheap check used to set the
/// `lyrics_synced` column without threading the full parse result around.
pub fn is_synced(raw: &str) -> bool {
    raw.lines().any(|line| {
        let l = line.trim_start();
        l.strip_prefix('[')
            .and_then(|r| r.find(']').map(|c| &r[..c]))
            .is_some_and(|inner| parse_timestamp(inner).is_some())
    })
}

fn plain_lines_to_lines(plain: &str) -> Vec<LyricLine> {
    plain
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| LyricLine {
            ms: 0,
            text: l.to_string(),
        })
        .collect()
}

/// Parse a bracket's inner text as an `mm:ss[.frac]` timestamp → milliseconds.
///
/// Returns `None` for anything that isn't a timestamp (which the caller then
/// tries to interpret as a metadata tag). `mm` may exceed 59 (long tracks);
/// `ss` must be `< 60` so a metadata value like `length:03` isn't mistaken
/// for a stamp.
fn parse_timestamp(inner: &str) -> Option<i64> {
    let (mm_str, rest) = inner.split_once(':')?;
    let mm: i64 = mm_str.trim().parse().ok()?;
    if mm < 0 {
        return None;
    }
    let (ss_str, frac_str) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let ss_str = ss_str.trim();
    if ss_str.is_empty() || !ss_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ss: i64 = ss_str.parse().ok()?;
    if ss >= 60 {
        return None;
    }
    let mut ms = (mm * 60 + ss) * 1000;
    if let Some(frac) = frac_str {
        let frac = frac.trim();
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        // Centiseconds (2 digits) is the LRC norm; 3 digits = milliseconds.
        ms += match frac.len() {
            1 => frac.parse::<i64>().ok()? * 100,
            2 => frac.parse::<i64>().ok()? * 10,
            _ => frac[..3].parse::<i64>().ok()?,
        };
    }
    Some(ms)
}

/// Recognise an ID3-style metadata tag (`ar:artist`, `offset:+250`, …): the
/// key must be non-empty and all-alphabetic so numeric timestamps don't match.
fn split_meta(inner: &str) -> Option<(&str, &str)> {
    let (key, val) = inner.split_once(':')?;
    let key = key.trim();
    if !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphabetic()) {
        Some((key, val))
    } else {
        None
    }
}

/// Strip enhanced (word-level) `<mm:ss.xx>` inline stamps, leaving the words.
/// Only removes bracketed groups whose content parses as a timestamp so real
/// `<...>` text (rare, but possible) survives.
fn strip_word_tags(s: &str) -> String {
    if !s.contains('<') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '<'
            && let Some(close) = s[i + 1..].find('>')
            && parse_timestamp(&s[i + 1..i + 1 + close]).is_some()
        {
            // Skip past the closing '>'.
            while let Some(&(j, _)) = chars.peek() {
                if j <= i + 1 + close {
                    chars.next();
                } else {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    // Collapse the double-spaces that removing inline stamps can leave behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_synced() {
        let lrc = "[00:12.00]Line one\n[00:17.50]Line two\n[01:05.00]Line three";
        let p = parse(lrc);
        assert!(p.synced);
        assert_eq!(p.lines.len(), 3);
        assert_eq!(
            p.lines[0],
            LyricLine {
                ms: 12_000,
                text: "Line one".into()
            }
        );
        assert_eq!(
            p.lines[1],
            LyricLine {
                ms: 17_500,
                text: "Line two".into()
            }
        );
        assert_eq!(
            p.lines[2],
            LyricLine {
                ms: 65_000,
                text: "Line three".into()
            }
        );
        assert_eq!(p.plain, "Line one\nLine two\nLine three");
    }

    #[test]
    fn millisecond_precision() {
        let p = parse("[00:01.234]Precise");
        assert_eq!(p.lines[0].ms, 1_234);
    }

    #[test]
    fn multi_stamp_line_repeats_text() {
        let lrc = "[00:10.00][00:40.00][01:10.00]Chorus";
        let p = parse(lrc);
        assert_eq!(p.lines.len(), 3);
        assert_eq!(p.lines[0].ms, 10_000);
        assert_eq!(p.lines[1].ms, 40_000);
        assert_eq!(p.lines[2].ms, 70_000);
        assert!(p.lines.iter().all(|l| l.text == "Chorus"));
        // Plain text lists the repeated line once.
        assert_eq!(p.plain, "Chorus");
    }

    #[test]
    fn out_of_order_stamps_are_sorted() {
        let lrc = "[00:30.00]Second\n[00:10.00]First";
        let p = parse(lrc);
        assert_eq!(p.lines[0].text, "First");
        assert_eq!(p.lines[1].text, "Second");
    }

    #[test]
    fn honors_positive_offset_shifts_earlier() {
        // +500ms offset pulls every stamp 500ms earlier.
        let p = parse("[offset:+500]\n[00:10.00]Line");
        assert_eq!(p.lines[0].ms, 9_500);
    }

    #[test]
    fn honors_negative_offset_shifts_later() {
        let p = parse("[offset:-500]\n[00:10.00]Line");
        assert_eq!(p.lines[0].ms, 10_500);
    }

    #[test]
    fn offset_clamps_at_zero() {
        let p = parse("[offset:+20000]\n[00:10.00]Line");
        assert_eq!(p.lines[0].ms, 0);
    }

    #[test]
    fn skips_metadata_headers() {
        let lrc = "[ar:Mara Vesper]\n[ti:Halcyon Drift]\n[al:Halcyon Drift]\n[length:03:42]\n[00:06.00]VERSE";
        let p = parse(lrc);
        assert!(p.synced);
        assert_eq!(p.lines.len(), 1);
        assert_eq!(p.lines[0].text, "VERSE");
        assert_eq!(p.plain, "VERSE");
    }

    #[test]
    fn plain_lyrics_detected() {
        let lrc = "Just some words\n\nAnd more words";
        let p = parse(lrc);
        assert!(!p.synced);
        assert_eq!(p.lines.len(), 2);
        assert!(p.lines.iter().all(|l| l.ms == 0));
        assert_eq!(p.plain, "Just some words\nAnd more words");
    }

    #[test]
    fn empty_timestamped_line_kept_for_timing() {
        // An empty synced line (instrumental gap) keeps its stamp but adds no
        // plain text.
        let lrc = "[00:05.00]\n[00:10.00]Words";
        let p = parse(lrc);
        assert_eq!(p.lines.len(), 2);
        assert_eq!(
            p.lines[0],
            LyricLine {
                ms: 5_000,
                text: String::new()
            }
        );
        assert_eq!(p.plain, "Words");
    }

    #[test]
    fn enhanced_word_tags_stripped_to_line() {
        let lrc = "[00:10.00]<00:10.00>Hold <00:10.40>my <00:10.90>hand";
        let p = parse(lrc);
        assert_eq!(p.lines[0].text, "Hold my hand");
    }

    #[test]
    fn crlf_line_endings() {
        let p = parse("[00:01.00]One\r\n[00:02.00]Two\r\n");
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[1].text, "Two");
    }

    #[test]
    fn minutes_over_59() {
        let p = parse("[75:00.00]Long track");
        assert_eq!(p.lines[0].ms, 75 * 60 * 1000);
    }

    #[test]
    fn is_synced_matches_parse() {
        assert!(is_synced("[00:01.00]hi"));
        assert!(!is_synced("just text"));
        assert!(!is_synced("[ar:only metadata]"));
    }

    #[test]
    fn lyric_opening_with_bracket_is_text() {
        // "[incomplete" has no matching timestamp/meta → treated as text.
        let p = parse("[00:01.00][bracketed] words");
        assert_eq!(p.lines[0].text, "[bracketed] words");
    }
}
