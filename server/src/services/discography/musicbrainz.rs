//! MusicBrainz implementation of [`DiscographyProvider`].
//!
//! Talks to `musicbrainz.org/ws/2` (`fmt=json`), reusing the shared `User-Agent`
//! + 1 req/s [`RateLimiter`] from [`crate::services::musicbrainz`]. Parsing is
//! `serde_json::Value`-based (like the artwork fetch) — tolerant of the fields
//! we don't use.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::services::musicbrainz::{RateLimiter, WS2_BASE, user_agent};

use super::provider::{ArtistCandidate, DiscographyProvider, ProviderReleaseGroup, ProviderTrack};

/// Max release-group pages to walk per artist (100/page) — a runaway backstop.
const MAX_RG_PAGES: usize = 25;

pub struct MusicBrainzDiscography {
    client: reqwest::Client,
    limiter: RateLimiter,
}

impl MusicBrainzDiscography {
    pub fn new(contact: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent(contact.as_deref()))
            .build()
            .expect("reqwest client build");
        Self {
            client,
            limiter: RateLimiter::per_second(),
        }
    }

    /// GET a `ws/2` endpoint as JSON, respecting the rate limit.
    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.limiter.acquire().await;
        let resp = self
            .client
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("musicbrainz request: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "musicbrainz status {} for {url}",
                resp.status()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| AppError::Internal(format!("musicbrainz json: {e}")))
    }
}

#[async_trait]
impl DiscographyProvider for MusicBrainzDiscography {
    fn id(&self) -> &str {
        "musicbrainz"
    }

    async fn resolve_artist(
        &self,
        name: &str,
        _hint_titles: &[String],
    ) -> Result<Vec<ArtistCandidate>> {
        // MB's Lucene query; strip embedded quotes so the phrase stays valid.
        let query = format!("artist:\"{}\"", name.replace('"', " "));
        let url = format!("{WS2_BASE}/artist");
        let body = self
            .get_json(
                &url,
                &[("query", query.as_str()), ("fmt", "json"), ("limit", "8")],
            )
            .await?;
        let mut out = Vec::new();
        if let Some(arr) = body.get("artists").and_then(|v| v.as_array()) {
            for a in arr {
                let Some(id) = a.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let name = a
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                // `score` may be a number or a stringified number depending on
                // the MB version; accept either.
                let score = a
                    .get("score")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(0)
                    .clamp(0, 100) as u8;
                out.push(ArtistCandidate {
                    provider_id: id.to_string(),
                    name,
                    disambiguation: a
                        .get("disambiguation")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    score,
                });
            }
        }
        Ok(out)
    }

    async fn release_groups(&self, provider_artist_id: &str) -> Result<Vec<ProviderReleaseGroup>> {
        let url = format!("{WS2_BASE}/release-group");
        let mut out = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_RG_PAGES {
            let offset_s = offset.to_string();
            let body = self
                .get_json(
                    &url,
                    &[
                        ("artist", provider_artist_id),
                        ("fmt", "json"),
                        ("limit", "100"),
                        ("offset", offset_s.as_str()),
                    ],
                )
                .await?;
            let arr = body
                .get("release-groups")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let page_len = arr.len();
            for rg in arr {
                let Some(id) = rg.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let title = rg
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let primary = rg.get("primary-type").and_then(|v| v.as_str());
                let secondary: Vec<String> = rg
                    .get("secondary-types")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ProviderReleaseGroup {
                    provider_id: id.to_string(),
                    title,
                    album_type: map_album_type(primary, &secondary),
                    year: rg
                        .get("first-release-date")
                        .and_then(|v| v.as_str())
                        .and_then(parse_year),
                });
            }
            let total = body
                .get("release-group-count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            offset += 100;
            if page_len < 100 || offset >= total {
                break;
            }
        }
        Ok(out)
    }

    async fn tracklist(&self, provider_release_group_id: &str) -> Result<Vec<ProviderTrack>> {
        // 1. Enumerate the group's releases and pick the canonical one (§4.5).
        let rg_url = format!("{WS2_BASE}/release-group/{provider_release_group_id}");
        let rg_body = self
            .get_json(&rg_url, &[("inc", "releases"), ("fmt", "json")])
            .await?;
        let releases = rg_body
            .get("releases")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let Some(release_id) = pick_canonical_release(&releases) else {
            return Ok(Vec::new());
        };

        // 2. Fetch that release's media + recordings.
        let rel_url = format!("{WS2_BASE}/release/{release_id}");
        let rel_body = self
            .get_json(&rel_url, &[("inc", "recordings"), ("fmt", "json")])
            .await?;
        let mut out = Vec::new();
        if let Some(media) = rel_body.get("media").and_then(|v| v.as_array()) {
            for (mi, medium) in media.iter().enumerate() {
                let disc_no = medium
                    .get("position")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i32)
                    .or(Some(mi as i32 + 1));
                if let Some(tracks) = medium.get("tracks").and_then(|v| v.as_array()) {
                    for t in tracks {
                        let title = t
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if title.is_empty() {
                            continue;
                        }
                        let position = t.get("position").and_then(|v| v.as_i64()).map(|n| n as i32);
                        let recording_id = t
                            .get("recording")
                            .and_then(|r| r.get("id"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        out.push(ProviderTrack {
                            provider_id: recording_id,
                            position,
                            disc_no,
                            title,
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Map MB `primary-type` + `secondary-types` → our `album_type` (§4.4).
/// Secondary "live" wins; compilations/soundtracks/etc. become `other` (excluded
/// by default via `DISCOGRAPHY_INCLUDE_TYPES`).
fn map_album_type(primary: Option<&str>, secondary: &[String]) -> String {
    if secondary.iter().any(|s| s == "live") {
        return "live".to_string();
    }
    if secondary.iter().any(|s| {
        matches!(
            s.as_str(),
            "compilation"
                | "soundtrack"
                | "remix"
                | "dj-mix"
                | "mixtape/street"
                | "interview"
                | "demo"
                | "audio drama"
                | "field recording"
                | "spokenword"
        )
    }) {
        return "other".to_string();
    }
    match primary.map(|p| p.to_ascii_lowercase()).as_deref() {
        Some("album") => "album".to_string(),
        Some("ep") => "ep".to_string(),
        Some("single") => "single".to_string(),
        _ => "other".to_string(),
    }
}

/// Choose the canonical release of a release-group: prefer `status = Official`,
/// then the earliest date. Returns its MBID.
fn pick_canonical_release(releases: &[Value]) -> Option<String> {
    let mut best: Option<(&str, bool, String)> = None; // (id, is_official, date)
    for r in releases {
        let Some(id) = r.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let is_official = r
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("Official"))
            .unwrap_or(false);
        // Missing date sorts last (use a high sentinel).
        let date = r
            .get("date")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("9999")
            .to_string();
        let better = match &best {
            None => true,
            Some((_, best_official, best_date)) => {
                // Official beats non-official; then earlier date wins.
                (is_official, std::cmp::Reverse(&date))
                    > (*best_official, std::cmp::Reverse(best_date))
            }
        };
        if better {
            best = Some((id, is_official, date));
        }
    }
    best.map(|(id, _, _)| id.to_string())
}

/// Parse a leading `YYYY` out of a MusicBrainz date (`"1998"`, `"1998-05-01"`).
fn parse_year(date: &str) -> Option<i32> {
    let head: String = date.chars().take(4).collect();
    if head.len() == 4 {
        head.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_types() {
        assert_eq!(map_album_type(Some("Album"), &[]), "album");
        assert_eq!(map_album_type(Some("EP"), &[]), "ep");
        assert_eq!(map_album_type(Some("Single"), &[]), "single");
        assert_eq!(map_album_type(Some("Album"), &["live".to_string()]), "live");
        assert_eq!(
            map_album_type(Some("Album"), &["compilation".to_string()]),
            "other"
        );
    }

    #[test]
    fn canonical_prefers_official_then_earliest() {
        let releases = vec![
            json!({"id": "a", "status": "Official", "date": "2005"}),
            json!({"id": "b", "status": "Official", "date": "1999"}),
            json!({"id": "c", "status": "Bootleg", "date": "1998"}),
        ];
        assert_eq!(pick_canonical_release(&releases).as_deref(), Some("b"));
    }

    #[test]
    fn parses_years() {
        assert_eq!(parse_year("1998-05-01"), Some(1998));
        assert_eq!(parse_year("1998"), Some(1998));
        assert_eq!(parse_year(""), None);
    }
}
