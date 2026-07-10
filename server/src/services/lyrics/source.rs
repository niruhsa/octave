//! External lyric providers.
//!
//! A trait so the network provider is swappable + unit-testable without a live
//! service (mirrors [`CoverArtSource`](crate::services::artwork::CoverArtSource)).
//! [`LrcLibSource`] is the default, backed by the free, no-API-key
//! [LRCLIB](https://lrclib.net) database.

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{AppError, Result};

/// The lookup key for a track's lyrics.
#[derive(Debug, Clone)]
pub struct LyricQuery<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: &'a str,
    pub duration_secs: u32,
}

/// A provider result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricResult {
    /// An `.lrc` (synced) or plain-text hit; `synced` reflects which.
    Lyrics { text: String, synced: bool },
    /// A positive "this track has no lyrics" — recorded so it isn't refetched.
    Instrumental,
}

/// Look up lyrics for a track. Isolated behind a trait so tests inject a fake.
#[async_trait]
pub trait LyricsSource: Send + Sync {
    /// `Ok(None)` = not found (retryable); `Ok(Some(..))` = a definitive result.
    async fn fetch(&self, q: &LyricQuery<'_>) -> Result<Option<LyricResult>>;
}

/// User-Agent LRCLIB asks external clients to send (app + version + optional
/// operator contact), matching the artwork / MusicBrainz clients' etiquette.
fn user_agent(contact: Option<&str>) -> String {
    match contact {
        Some(c) if !c.trim().is_empty() => format!(
            "music-server/{} ( {} )",
            env!("CARGO_PKG_VERSION"),
            c.trim()
        ),
        _ => format!(
            "music-server/{} ( https://github.com/ )",
            env!("CARGO_PKG_VERSION")
        ),
    }
}

/// [LRCLIB](https://lrclib.net) source — free, open, no API key.
pub struct LrcLibSource {
    client: reqwest::Client,
    /// Base URL (overridable in tests to hit a local fake server).
    base_url: String,
}

impl LrcLibSource {
    pub fn new(contact: Option<&str>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent(contact))
            .build()
            .expect("reqwest client build");
        Self {
            client,
            base_url: "https://lrclib.net".to_string(),
        }
    }

    /// Point the source at a different base URL (test fake server).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// LRCLIB's track object (both `/get` and each `/search` array element).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibTrack {
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
}

/// Turn an LRCLIB track into a definitive result, or `None` when it carries no
/// usable lyrics (so the caller can try the next candidate / fall through).
fn to_result(t: &LrcLibTrack) -> Option<LyricResult> {
    if t.instrumental {
        return Some(LyricResult::Instrumental);
    }
    if let Some(s) = t.synced_lyrics.as_ref().filter(|s| !s.trim().is_empty()) {
        return Some(LyricResult::Lyrics {
            text: s.clone(),
            synced: true,
        });
    }
    if let Some(p) = t.plain_lyrics.as_ref().filter(|s| !s.trim().is_empty()) {
        return Some(LyricResult::Lyrics {
            text: p.clone(),
            synced: false,
        });
    }
    None
}

/// From a `/search` candidate list, pick the track whose duration is closest to
/// the query — LRCLIB's fuzzy fallback when the exact `/get` misses.
fn closest(cands: &[LrcLibTrack], want_secs: u32) -> Option<&LrcLibTrack> {
    cands.iter().min_by_key(|t| {
        let secs = t.duration.round().max(0.0) as i64;
        (secs - want_secs as i64).abs()
    })
}

#[async_trait]
impl LyricsSource for LrcLibSource {
    async fn fetch(&self, q: &LyricQuery<'_>) -> Result<Option<LyricResult>> {
        // 1. Exact match by artist + title + album + duration.
        let duration = q.duration_secs.to_string();
        let get_url = format!("{}/api/get", self.base_url);
        let resp = self
            .client
            .get(&get_url)
            .query(&[
                ("artist_name", q.artist),
                ("track_name", q.title),
                ("album_name", q.album),
                ("duration", duration.as_str()),
            ])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("lrclib get: {e}")))?;
        if resp.status().is_success() {
            let track: LrcLibTrack = resp
                .json()
                .await
                .map_err(|e| AppError::Internal(format!("lrclib get json: {e}")))?;
            if let Some(r) = to_result(&track) {
                return Ok(Some(r));
            }
            // A hit with no usable lyrics — fall through to search.
        } else if resp.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(AppError::Internal(format!(
                "lrclib get status {}",
                resp.status()
            )));
        }

        // 2. Fuzzy fallback: search by title + artist, pick the closest duration.
        let search_url = format!("{}/api/search", self.base_url);
        let resp = self
            .client
            .get(&search_url)
            .query(&[("track_name", q.title), ("artist_name", q.artist)])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("lrclib search: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "lrclib search status {}",
                resp.status()
            )));
        }
        let cands: Vec<LrcLibTrack> = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("lrclib search json: {e}")))?;
        Ok(closest(&cands, q.duration_secs).and_then(to_result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(instrumental: bool, plain: Option<&str>, synced: Option<&str>) -> LrcLibTrack {
        LrcLibTrack {
            duration: 180.0,
            instrumental,
            plain_lyrics: plain.map(str::to_string),
            synced_lyrics: synced.map(str::to_string),
        }
    }

    #[test]
    fn prefers_synced_over_plain() {
        let t = mk(false, Some("plain text"), Some("[00:01.00]synced"));
        assert_eq!(
            to_result(&t),
            Some(LyricResult::Lyrics {
                text: "[00:01.00]synced".into(),
                synced: true
            })
        );
    }

    #[test]
    fn falls_back_to_plain() {
        let t = mk(false, Some("plain text"), None);
        assert_eq!(
            to_result(&t),
            Some(LyricResult::Lyrics {
                text: "plain text".into(),
                synced: false
            })
        );
    }

    #[test]
    fn honors_instrumental() {
        let t = mk(true, None, None);
        assert_eq!(to_result(&t), Some(LyricResult::Instrumental));
    }

    #[test]
    fn empty_lyrics_is_none() {
        assert_eq!(to_result(&mk(false, Some("   "), Some(""))), None);
        assert_eq!(to_result(&mk(false, None, None)), None);
    }

    #[test]
    fn closest_duration_wins() {
        let cands = vec![
            LrcLibTrack {
                duration: 100.0,
                instrumental: false,
                plain_lyrics: Some("far".into()),
                synced_lyrics: None,
            },
            LrcLibTrack {
                duration: 182.0,
                instrumental: false,
                plain_lyrics: Some("near".into()),
                synced_lyrics: None,
            },
        ];
        let picked = closest(&cands, 180).unwrap();
        assert_eq!(picked.plain_lyrics.as_deref(), Some("near"));
    }
}
