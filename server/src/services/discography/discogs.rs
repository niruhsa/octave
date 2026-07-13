//! Discogs implementation of [`DiscographyProvider`] (Phase D — multi-provider).
//!
//! Talks to `api.discogs.com`, reusing the shared 1 req/s [`RateLimiter`] +
//! required `User-Agent` from [`crate::services::musicbrainz`]. A personal-access
//! token (`DISCOGRAPHY_DISCOGS_TOKEN`) authenticates search and raises the rate
//! limit (60/min vs 25/min); without it, artist **search** returns 401 (the
//! `/artists` + `/masters` reads are public).
//!
//! Discogs models a discography as **masters** (logical releases, ≈ MusicBrainz
//! release-groups) plus concrete **releases** (editions). We report an artist's
//! `Main`-role `master` entries as release-groups and read a master's tracklist.
//! Discogs search returns no relevance score, so we derive one from name
//! similarity — giving the service's confidence policy (§4.2) a signal to work
//! with. Provider ids are Discogs integers rendered as strings (which is exactly
//! why Phase D made the stored ids provider-agnostic TEXT).

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::services::musicbrainz::{RateLimiter, user_agent};

use super::r#match::{normalize_title, similarity};
use super::provider::{ArtistCandidate, DiscographyProvider, ProviderReleaseGroup, ProviderTrack};

const DISCOGS_BASE: &str = "https://api.discogs.com";
/// Max release pages to walk per artist (100/page) — a runaway backstop.
const MAX_RELEASE_PAGES: usize = 25;

pub struct DiscogsDiscography {
    client: reqwest::Client,
    limiter: RateLimiter,
    /// Personal-access token. `None` → unauthenticated (search will 401).
    token: Option<String>,
}

impl DiscogsDiscography {
    pub fn new(token: Option<String>, contact: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent(contact.as_deref()))
            .build()
            .expect("reqwest client build");
        Self {
            client,
            limiter: RateLimiter::per_second(),
            token,
        }
    }

    async fn get_json(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.limiter.acquire().await;
        let mut req = self.client.get(url).query(query);
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Discogs token={token}"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("discogs request: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "discogs status {} for {url}",
                resp.status()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| AppError::Internal(format!("discogs json: {e}")))
    }
}

#[async_trait]
impl DiscographyProvider for DiscogsDiscography {
    fn id(&self) -> &str {
        "discogs"
    }

    async fn resolve_artist(
        &self,
        name: &str,
        _hint_titles: &[String],
    ) -> Result<Vec<ArtistCandidate>> {
        let url = format!("{DISCOGS_BASE}/database/search");
        let body = self
            .get_json(&url, &[("q", name), ("type", "artist"), ("per_page", "8")])
            .await?;
        let want = normalize_title(name);
        let mut out = Vec::new();
        if let Some(arr) = body.get("results").and_then(|v| v.as_array()) {
            for r in arr {
                let Some(id) = r.get("id").and_then(|v| v.as_i64()) else {
                    continue;
                };
                let title = r
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                // Discogs gives no relevance score — derive one from name
                // similarity so the confidence policy has something to gate on.
                let score = (similarity(&want, &normalize_title(&title)) * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8;
                out.push(ArtistCandidate {
                    provider_id: id.to_string(),
                    name: title,
                    disambiguation: None,
                    score,
                });
            }
        }
        // Best score first (the service expects ranked candidates).
        out.sort_by(|a, b| b.score.cmp(&a.score));
        Ok(out)
    }

    async fn release_groups(&self, provider_artist_id: &str) -> Result<Vec<ProviderReleaseGroup>> {
        let url = format!("{DISCOGS_BASE}/artists/{provider_artist_id}/releases");
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let page_s = page.to_string();
            let body = self
                .get_json(
                    &url,
                    &[
                        ("per_page", "100"),
                        ("page", page_s.as_str()),
                        ("sort", "year"),
                        ("sort_order", "asc"),
                    ],
                )
                .await?;
            if let Some(arr) = body.get("releases").and_then(|v| v.as_array()) {
                for it in arr {
                    // Only the artist's own logical releases (masters), not
                    // appearances/credits or individual editions.
                    let role = it.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    let typ = it.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if role != "Main" || typ != "master" {
                        continue;
                    }
                    let Some(id) = it.get("id").and_then(|v| v.as_i64()) else {
                        continue;
                    };
                    let title = it
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let year = it
                        .get("year")
                        .and_then(|v| v.as_i64())
                        .filter(|y| *y > 0)
                        .map(|y| y as i32);
                    let format = it.get("format").and_then(|v| v.as_str()).unwrap_or("");
                    out.push(ProviderReleaseGroup {
                        provider_id: id.to_string(),
                        title,
                        album_type: map_album_type(format),
                        year,
                    });
                }
            }
            let pages = body
                .get("pagination")
                .and_then(|p| p.get("pages"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            page += 1;
            if page > pages || page > MAX_RELEASE_PAGES {
                break;
            }
        }
        Ok(out)
    }

    async fn tracklist(&self, provider_release_group_id: &str) -> Result<Vec<ProviderTrack>> {
        let url = format!("{DISCOGS_BASE}/masters/{provider_release_group_id}");
        let body = self.get_json(&url, &[]).await?;
        let mut out = Vec::new();
        if let Some(arr) = body.get("tracklist").and_then(|v| v.as_array()) {
            for (i, t) in arr.iter().enumerate() {
                // Skip headings / index entries (only real tracks have
                // `type_ == "track"`, or no type_ at all on older data).
                if let Some(ty) = t.get("type_").and_then(|v| v.as_str()) {
                    if ty != "track" {
                        continue;
                    }
                }
                let title = t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if title.is_empty() {
                    continue;
                }
                let position = t
                    .get("position")
                    .and_then(|v| v.as_str())
                    .and_then(parse_leading_int)
                    .or(Some(i as i32 + 1));
                out.push(ProviderTrack {
                    provider_id: None,
                    position,
                    disc_no: None,
                    title,
                });
            }
        }
        Ok(out)
    }
}

/// Map a Discogs `format` hint to our `album_type`. Coarser than MusicBrainz
/// (Discogs masters are format-agnostic, so this is best-effort): a missing
/// format defaults to `album`.
fn map_album_type(format: &str) -> String {
    let f = format.to_ascii_lowercase();
    if f.contains("compilation") {
        "other".to_string()
    } else if f.contains("single") {
        "single".to_string()
    } else if f.contains("ep") {
        "ep".to_string()
    } else {
        "album".to_string()
    }
}

/// Parse a leading integer out of a Discogs track position (`"1"`, `"A1"`,
/// `"2-3"`) — `None` when it doesn't start with a digit.
fn parse_leading_int(pos: &str) -> Option<i32> {
    let digits: String = pos.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_formats() {
        assert_eq!(map_album_type("Album"), "album");
        assert_eq!(map_album_type(""), "album");
        assert_eq!(map_album_type("7\", Single"), "single");
        assert_eq!(map_album_type("CD, EP"), "ep");
        assert_eq!(map_album_type("CD, Compilation"), "other");
    }

    #[test]
    fn parses_positions() {
        assert_eq!(parse_leading_int("1"), Some(1));
        assert_eq!(parse_leading_int("12"), Some(12));
        assert_eq!(parse_leading_int("A1"), None);
        assert_eq!(parse_leading_int(""), None);
    }
}
