//! AcoustID audio-anchored resolution (Phase E) — behind the `chromaprint`
//! feature.
//!
//! Resolves an artist to a MusicBrainz id from the **audio** of a few owned
//! tracks: each track's stored Chromaprint (hex-encoded raw `u32`s, computed
//! with the `test2` algorithm — AcoustID's) is compressed to AcoustID's binary
//! format via [`FingerprintCompressor`] + URL-safe base64, then submitted to
//! `api.acoustid.org/v2/lookup` with the track duration. The returned recordings
//! carry MusicBrainz artist ids; [`super::service::dominant_artist`] picks the
//! one the tracks agree on.
//!
//! Rate-limited (~3/s, AcoustID's limit). Needs a `DISCOGRAPHY_ACOUSTID_KEY`.
//! Everything degrades gracefully: a lookup failure or a fingerprint AcoustID
//! doesn't recognise just means the caller falls back to name search.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rusty_chromaprint::{Configuration, FingerprintCompressor};
use serde_json::Value;

use crate::config::DiscographyConfig;
use crate::db::models::TrackFingerprint;
use crate::error::{AppError, Result};
use crate::services::musicbrainz::{user_agent, RateLimiter};

use super::service::{dominant_artist, AudioResolver};

const ACOUSTID_LOOKUP: &str = "https://api.acoustid.org/v2/lookup";

pub struct AcoustIdResolver {
    client: reqwest::Client,
    limiter: RateLimiter,
    key: String,
}

impl AcoustIdResolver {
    pub fn new(key: String, contact: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(user_agent(contact.as_deref()))
            .build()
            .expect("reqwest client build");
        // AcoustID allows ~3 requests/second.
        Self {
            client,
            limiter: RateLimiter::new(Duration::from_millis(340)),
            key,
        }
    }

    /// Look up one fingerprint → `(artist MBIDs of the best result, its score)`.
    async fn lookup(&self, fingerprint: &str, duration_secs: i64) -> Result<(Vec<String>, f32)> {
        self.limiter.acquire().await;
        let dur = duration_secs.to_string();
        let resp = self
            .client
            .get(ACOUSTID_LOOKUP)
            .query(&[
                ("client", self.key.as_str()),
                ("format", "json"),
                ("duration", dur.as_str()),
                ("meta", "recordings"),
                ("fingerprint", fingerprint),
            ])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("acoustid request: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "acoustid status {}",
                resp.status()
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("acoustid json: {e}")))?;

        // Take the highest-scoring result and collect its recordings' artist ids.
        let mut best: Option<(f32, Vec<String>)> = None;
        if let Some(results) = body.get("results").and_then(|v| v.as_array()) {
            for r in results {
                let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let mut artists = Vec::new();
                if let Some(recs) = r.get("recordings").and_then(|v| v.as_array()) {
                    for rec in recs {
                        if let Some(arts) = rec.get("artists").and_then(|v| v.as_array()) {
                            for a in arts {
                                if let Some(id) = a.get("id").and_then(|v| v.as_str()) {
                                    artists.push(id.to_string());
                                }
                            }
                        }
                    }
                }
                if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                    best = Some((score, artists));
                }
            }
        }
        Ok(best.map(|(s, a)| (a, s)).unwrap_or((Vec::new(), 0.0)))
    }
}

#[async_trait]
impl AudioResolver for AcoustIdResolver {
    async fn resolve_artist(&self, fingerprints: &[TrackFingerprint]) -> Result<Option<String>> {
        let mut tracks: Vec<(Vec<String>, f32)> = Vec::new();
        for fp in fingerprints {
            let Some(encoded) = encode_fingerprint(&fp.chromaprint) else {
                continue;
            };
            let secs = (fp.duration_ms / 1000).max(1);
            match self.lookup(&encoded, secs).await {
                Ok((artists, score)) => tracks.push((artists, score)),
                Err(e) => tracing::debug!(error = %e, "acoustid lookup failed"),
            }
        }
        Ok(dominant_artist(&tracks))
    }
}

/// Build the resolver, or `None` when it isn't applicable: needs an AcoustID key
/// and the MusicBrainz provider (AcoustID maps to MusicBrainz ids).
pub(super) fn build(cfg: &DiscographyConfig) -> Option<Arc<dyn AudioResolver>> {
    let key = cfg.acoustid_key.clone()?;
    if cfg.provider != "musicbrainz" {
        return None;
    }
    Some(Arc::new(AcoustIdResolver::new(key, cfg.contact.clone())))
}

/// Our stored Chromaprint (hex of raw `u32`s, `test2` algorithm) → AcoustID's
/// submission format: chromaprint-compressed bytes, URL-safe base64 (no pad).
/// `None` on malformed input.
fn encode_fingerprint(hex: &str) -> Option<String> {
    if hex.is_empty() || hex.len() % 8 != 0 {
        return None;
    }
    let mut raw = Vec::with_capacity(hex.len() / 8);
    for chunk in hex.as_bytes().chunks(8) {
        let s = std::str::from_utf8(chunk).ok()?;
        raw.push(u32::from_str_radix(s, 16).ok()?);
    }
    // Compress with the same algorithm the fingerprints were computed with
    // (`test2`) so the header's algorithm byte matches AcoustID's index.
    let compressed = FingerprintCompressor::from(&Configuration::preset_test2()).compress(&raw);
    Some(base64_url_nopad(&compressed))
}

/// URL-safe base64 (`-_`, no padding) — AcoustID's fingerprint encoding.
fn base64_url_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_url_nopad(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_url_nopad(b"fo"), "Zm8"); // no padding
        assert_eq!(base64_url_nopad(b""), "");
    }

    #[test]
    fn encode_fingerprint_round_trips_and_rejects_garbage() {
        // Two raw words → 16 hex chars → a stable non-empty encoding.
        let hex = "0000000112345678";
        let a = encode_fingerprint(hex).expect("valid hex encodes");
        let b = encode_fingerprint(hex).expect("deterministic");
        assert_eq!(a, b);
        assert!(!a.is_empty());
        // Malformed lengths / non-hex → None.
        assert_eq!(encode_fingerprint("123"), None);
        assert_eq!(encode_fingerprint(""), None);
        assert_eq!(encode_fingerprint("zzzzzzzz"), None);
    }
}
