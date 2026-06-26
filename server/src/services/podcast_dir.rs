//! Podcast directory (discovery) sources.
//!
//! Search shows by free text and resolve a directory id to a feed URL. The
//! external calls are isolated behind the [`PodcastDirectory`] trait so the
//! [`crate::services::PodcastService`] depends on an interface and is testable
//! with a fake — mirroring [`crate::services::CoverArtSource`].
//!
//! Two implementations:
//! - [`ItunesDirectory`] — the **iTunes Search API**, always available (no key).
//! - [`PodcastIndexDirectory`] — the **PodcastIndex API**, optional (gated by
//!   `PODCASTINDEX_API_KEY` + `PODCASTINDEX_API_SECRET`), with an iTunes
//!   fallback when it errors or returns nothing.

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tracing::warn;

use crate::error::{AppError, Result};

const USER_AGENT: &str = concat!("music-server/", env!("CARGO_PKG_VERSION"), " ( podcasts )");

/// A lightweight show candidate from a directory search — enough to subscribe
/// (the feed URL) plus display metadata. Episodes come from parsing the feed.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PodcastCandidate {
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
}

/// A podcast directory: search shows + resolve a directory id to a feed.
#[async_trait]
pub trait PodcastDirectory: Send + Sync {
    /// Search shows by free text. Returns candidates (feed URL + display
    /// metadata), **not** episodes.
    async fn search(&self, term: &str, limit: i64) -> Result<Vec<PodcastCandidate>>;
    /// Resolve a directory id (iTunes collectionId) to a candidate, or `None`.
    async fn lookup(&self, id: i64) -> Result<Option<PodcastCandidate>>;
}

// ===========================================================================
// iTunes Search API (no key)
// ===========================================================================

pub struct ItunesDirectory {
    client: reqwest::Client,
}

impl Default for ItunesDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl ItunesDirectory {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client build");
        Self { client }
    }
}

#[async_trait]
impl PodcastDirectory for ItunesDirectory {
    async fn search(&self, term: &str, limit: i64) -> Result<Vec<PodcastCandidate>> {
        let limit = limit.clamp(1, 200).to_string();
        let resp = self
            .client
            .get("https://itunes.apple.com/search")
            .query(&[
                ("media", "podcast"),
                ("entity", "podcast"),
                ("term", term),
                ("limit", limit.as_str()),
            ])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("itunes search: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "itunes search status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("itunes json: {e}")))?;
        Ok(itunes_results_to_candidates(&body))
    }

    async fn lookup(&self, id: i64) -> Result<Option<PodcastCandidate>> {
        let resp = self
            .client
            .get("https://itunes.apple.com/lookup")
            .query(&[("id", id.to_string().as_str()), ("entity", "podcast")])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("itunes lookup: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "itunes lookup status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("itunes json: {e}")))?;
        Ok(itunes_results_to_candidates(&body).into_iter().next())
    }
}

/// Map an iTunes Search/Lookup JSON body to candidates (skips results with no
/// `feedUrl` — they can't be subscribed). Pure, so it's unit-tested directly.
fn itunes_results_to_candidates(body: &Value) -> Vec<PodcastCandidate> {
    body.get("results")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(itunes_result_to_candidate).collect())
        .unwrap_or_default()
}

fn itunes_result_to_candidate(r: &Value) -> Option<PodcastCandidate> {
    let feed_url = r.get("feedUrl")?.as_str()?.to_string();
    if feed_url.trim().is_empty() {
        return None;
    }
    Some(PodcastCandidate {
        feed_url,
        title: r
            .get("collectionName")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        author: r.get("artistName").and_then(|v| v.as_str()).map(String::from),
        description: None,
        image_url: r
            .get("artworkUrl600")
            .or_else(|| r.get("artworkUrl100"))
            .or_else(|| r.get("artworkUrl60"))
            .and_then(|v| v.as_str())
            .map(String::from),
        categories: r
            .get("genres")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|g| g.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        itunes_id: r
            .get("collectionId")
            .or_else(|| r.get("trackId"))
            .and_then(|v| v.as_i64()),
        podcastindex_id: None,
    })
}

// ===========================================================================
// PodcastIndex API (optional — gated behind PODCASTINDEX_API_KEY/SECRET)
// ===========================================================================

pub struct PodcastIndexDirectory {
    client: reqwest::Client,
    api_key: String,
    api_secret: String,
    /// iTunes fallback when PodcastIndex errors or returns nothing.
    fallback: ItunesDirectory,
}

impl PodcastIndexDirectory {
    pub fn new(api_key: String, api_secret: String, fallback: ItunesDirectory) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client build");
        Self {
            client,
            api_key,
            api_secret,
            fallback,
        }
    }

    /// PodcastIndex auth: `X-Auth-Key`, `X-Auth-Date` (unix secs), and
    /// `Authorization = sha1(key + secret + date)`.
    fn auth_headers(&self) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let date = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let token = pi_auth_token(&self.api_key, &self.api_secret, date);
        let mut h = HeaderMap::new();
        let bad = |e| AppError::Internal(format!("podcastindex header: {e}"));
        h.insert("X-Auth-Key", HeaderValue::from_str(&self.api_key).map_err(bad)?);
        h.insert(
            "X-Auth-Date",
            HeaderValue::from_str(&date.to_string()).map_err(bad)?,
        );
        h.insert("Authorization", HeaderValue::from_str(&token).map_err(bad)?);
        Ok(h)
    }

    async fn pi_search(&self, term: &str, limit: i64) -> Result<Vec<PodcastCandidate>> {
        let max = limit.clamp(1, 200).to_string();
        let resp = self
            .client
            .get("https://api.podcastindex.org/api/1.0/search/byterm")
            .headers(self.auth_headers()?)
            .query(&[("q", term), ("max", max.as_str())])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("podcastindex search: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "podcastindex search status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("podcastindex json: {e}")))?;
        Ok(podcastindex_feeds_to_candidates(&body))
    }
}

#[async_trait]
impl PodcastDirectory for PodcastIndexDirectory {
    async fn search(&self, term: &str, limit: i64) -> Result<Vec<PodcastCandidate>> {
        match self.pi_search(term, limit).await {
            Ok(v) if !v.is_empty() => Ok(v),
            Ok(_) => self.fallback.search(term, limit).await,
            Err(e) => {
                warn!(error = %e, "podcastindex search failed; falling back to iTunes");
                self.fallback.search(term, limit).await
            }
        }
    }

    async fn lookup(&self, id: i64) -> Result<Option<PodcastCandidate>> {
        // The `id` here is an iTunes collectionId carried by an iTunes candidate;
        // resolve it via iTunes (PodcastIndex keys feeds by its own ids).
        self.fallback.lookup(id).await
    }
}

/// Map a PodcastIndex `search/byterm` body (`{ feeds: [...] }`) to candidates.
fn podcastindex_feeds_to_candidates(body: &Value) -> Vec<PodcastCandidate> {
    body.get("feeds")
        .and_then(|f| f.as_array())
        .map(|arr| arr.iter().filter_map(pi_feed_to_candidate).collect())
        .unwrap_or_default()
}

fn pi_feed_to_candidate(f: &Value) -> Option<PodcastCandidate> {
    let feed_url = f.get("url")?.as_str()?.to_string();
    if feed_url.trim().is_empty() {
        return None;
    }
    Some(PodcastCandidate {
        feed_url,
        title: f.get("title").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        author: f.get("author").and_then(|v| v.as_str()).map(String::from),
        description: f.get("description").and_then(|v| v.as_str()).map(String::from),
        image_url: f
            .get("artwork")
            .or_else(|| f.get("image"))
            .and_then(|v| v.as_str())
            .map(String::from),
        categories: f
            .get("categories")
            .and_then(|v| v.as_object())
            .map(|m| m.values().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        itunes_id: f.get("itunesId").and_then(|v| v.as_i64()),
        podcastindex_id: f.get("id").and_then(|v| v.as_i64()),
    })
}

/// `sha1(api_key + api_secret + unix_date)` as lowercase hex — the PodcastIndex
/// request signature.
fn pi_auth_token(key: &str, secret: &str, unix_date: u64) -> String {
    use sha1::{Digest, Sha1};
    use std::fmt::Write;
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(secret.as_bytes());
    hasher.update(unix_date.to_string().as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(40);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn itunes_mapping_skips_results_without_feed_url() {
        let body = serde_json::json!({
            "resultCount": 2,
            "results": [
                {
                    "collectionName": "Daily Tech",
                    "artistName": "Tech Media",
                    "feedUrl": "https://feeds.example.com/dailytech",
                    "artworkUrl600": "https://art.example.com/dt600.jpg",
                    "collectionId": 12345_i64,
                    "genres": ["Technology", "News"]
                },
                { "collectionName": "No Feed Here", "artistName": "Nobody" }
            ]
        });
        let cands = itunes_results_to_candidates(&body);
        assert_eq!(cands.len(), 1);
        let c = &cands[0];
        assert_eq!(c.feed_url, "https://feeds.example.com/dailytech");
        assert_eq!(c.title, "Daily Tech");
        assert_eq!(c.author.as_deref(), Some("Tech Media"));
        assert_eq!(c.itunes_id, Some(12345));
        assert_eq!(c.categories, vec!["Technology", "News"]);
        assert_eq!(c.image_url.as_deref(), Some("https://art.example.com/dt600.jpg"));
    }

    #[test]
    fn podcastindex_mapping() {
        let body = serde_json::json!({
            "status": "true",
            "feeds": [
                {
                    "id": 999_i64,
                    "title": "Index Show",
                    "author": "PI Author",
                    "description": "From the index.",
                    "url": "https://feeds.example.com/index",
                    "artwork": "https://art.example.com/idx.jpg",
                    "categories": { "9": "Tech", "55": "Science" }
                }
            ]
        });
        let cands = podcastindex_feeds_to_candidates(&body);
        assert_eq!(cands.len(), 1);
        let c = &cands[0];
        assert_eq!(c.feed_url, "https://feeds.example.com/index");
        assert_eq!(c.podcastindex_id, Some(999));
        assert_eq!(c.author.as_deref(), Some("PI Author"));
        assert_eq!(c.description.as_deref(), Some("From the index."));
        let mut cats = c.categories.clone();
        cats.sort();
        assert_eq!(cats, vec!["Science", "Tech"]);
    }

    #[test]
    fn auth_token_is_deterministic_40_char_hex() {
        let a = pi_auth_token("key", "secret", 1_700_000_000);
        let b = pi_auth_token("key", "secret", 1_700_000_000);
        assert_eq!(a, b, "same inputs → same token");
        assert_eq!(a.len(), 40, "sha1 hex is 40 chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Changing the date changes the signature.
        assert_ne!(a, pi_auth_token("key", "secret", 1_700_000_001));
    }

    #[test]
    fn auth_token_known_vector() {
        // SHA-1 of the concatenation "keysecret0" (key="key", secret="secret",
        // date=0), lowercase hex.
        assert_eq!(
            pi_auth_token("key", "secret", 0),
            "2fa7bc308a4de4eb9952552dbda4115f7985b603"
        );
    }
}
