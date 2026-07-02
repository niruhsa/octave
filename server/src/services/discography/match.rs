//! Title normalization + fuzzy matching (DISCOGRAPHY_SYNC.md §4.3).
//!
//! A single [`normalize_title`] drives every album/track compare so a
//! spelling/edition difference doesn't produce a false "missing". Kept
//! dependency-free (no `unicode-normalization` / `strsim` crate): a small
//! diacritic fold + a char-wise Levenshtein ratio are plenty for catalog titles
//! and keep the build lean.

/// Normalize a title into a comparison key: lowercased, de-accented, stripped of
/// parentheticals / bracketed qualifiers (`(Deluxe Edition)`, `[Explicit]`),
/// trailing edition suffixes (`- Remastered`), and `feat.` segments, with a
/// leading article dropped and punctuation removed.
pub fn normalize_title(input: &str) -> String {
    let lower = input.to_lowercase();
    let deaccented = deaccent(&lower);

    // Drop anything inside (), [], {} — edition/qualifier noise.
    let mut stripped = String::with_capacity(deaccented.len());
    let mut depth: i32 = 0;
    for c in deaccented.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ if depth == 0 => stripped.push(c),
            _ => {}
        }
    }

    // Drop a trailing "feat. …" / "featuring …" segment.
    for marker in [" feat.", " feat ", " featuring ", " ft.", " ft "] {
        if let Some(idx) = stripped.find(marker) {
            stripped.truncate(idx);
        }
    }

    // Drop a trailing " - <edition qualifier>" (only for recognised keywords, so
    // a real title with a dash like "song - part two" is preserved).
    if let Some(idx) = stripped.rfind(" - ") {
        let tail = stripped[idx + 3..].trim();
        let is_qualifier = tail == "single"
            || tail == "ep"
            || ["remaster", "deluxe", "edition", "version", "mono", "stereo", "remix", "mix"]
                .iter()
                .any(|k| tail.contains(k));
        if is_qualifier {
            stripped.truncate(idx);
        }
    }

    // Keep alphanumerics + spaces; everything else becomes a space.
    let mut cleaned = String::with_capacity(stripped.len());
    for c in stripped.chars() {
        if c.is_alphanumeric() {
            cleaned.push(c);
        } else if c.is_whitespace() {
            cleaned.push(' ');
        } else {
            cleaned.push(' ');
        }
    }

    // Drop a leading article and collapse whitespace.
    let mut tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if let Some(first) = tokens.first() {
        if matches!(*first, "the" | "a" | "an") && tokens.len() > 1 {
            tokens.remove(0);
        }
    }
    tokens.join(" ")
}

/// Similarity of two **already-normalized** titles in `[0.0, 1.0]` — a
/// char-wise Levenshtein ratio. `1.0` when equal, `0.0` when one is empty.
pub fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let dist = levenshtein(a, b);
    let max_len = a.chars().count().max(b.chars().count());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (dist as f32 / max_len as f32)
}

/// True when `provider` matches any of `local` keys: a normalized-equality hit,
/// else a fuzzy ratio at or above `threshold`. `provider` and every `local` key
/// must already be normalized (via [`normalize_title`]).
pub fn matches_any(provider: &str, local: &[String], threshold: f32) -> bool {
    if provider.is_empty() {
        return false;
    }
    for key in local {
        if key == provider {
            return true;
        }
        if similarity(provider, key) >= threshold {
            return true;
        }
    }
    false
}

/// Char-wise Levenshtein edit distance.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Fold common Latin-1 / Latin Extended-A accented letters to ASCII. Not
/// exhaustive Unicode NFKD — a pragmatic subset covering the accents seen in
/// Western catalog titles; unknown chars pass through unchanged.
fn deaccent(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => 'c',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ę' | 'ě' => 'e',
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' => 'i',
            'ñ' | 'ń' | 'ň' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' => 'u',
            'ý' | 'ÿ' => 'y',
            'ß' => 's',
            'š' | 'ś' => 's',
            'ž' | 'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_edition_parentheticals_and_articles() {
        assert_eq!(normalize_title("The Wall (Remastered 2011)"), "wall");
        assert_eq!(normalize_title("Album [Explicit]"), "album");
        assert_eq!(normalize_title("Songs - Deluxe Edition"), "songs");
        assert_eq!(normalize_title("Song (feat. Someone)"), "song");
        assert_eq!(normalize_title("Résumé"), "resume");
    }

    #[test]
    fn keeps_real_dashes() {
        // "part two" is not an edition keyword, so the dash segment survives.
        assert_eq!(normalize_title("Song - Part Two"), "song part two");
    }

    #[test]
    fn similarity_ranks_close_titles_high() {
        assert_eq!(similarity("wall", "wall"), 1.0);
        assert!(similarity("colour", "color") >= 0.8);
        assert!(similarity("abbey road", "wish you were here") < 0.5);
    }

    #[test]
    fn matches_any_uses_equality_then_fuzzy() {
        let local = vec!["dark side of the moon".to_string()];
        assert!(matches_any("dark side of the moon", &local, 0.9));
        assert!(!matches_any("animals", &local, 0.9));
    }
}
