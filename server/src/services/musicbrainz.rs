//! Shared MusicBrainz plumbing: the required `User-Agent` and a ~1 request/second
//! rate limiter.
//!
//! MusicBrainz enforces roughly one request per second per client. Both the
//! artwork fetch ([`super::artwork`]) and the discography sync
//! ([`super::discography`]) talk to `musicbrainz.org/ws/2`; a shared
//! [`RateLimiter`] lets them serialize onto that budget so a discography sync
//! and an artwork fetch don't trip the limit together.
//!
//! (Phase A wires the limiter into the discography provider; folding the
//! existing artwork client onto the same limiter is a small follow-up.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Base URL for the MusicBrainz XML Web Service v2 (JSON via `fmt=json`).
pub const WS2_BASE: &str = "https://musicbrainz.org/ws/2";

/// The `User-Agent` MusicBrainz (and the Cover Art Archive) require. `contact`
/// (an email or URL) is appended per MusicBrainz etiquette so they can reach the
/// operator of a busy client; `None` falls back to the project URL.
pub fn user_agent(contact: Option<&str>) -> String {
    match contact {
        Some(c) if !c.trim().is_empty() => {
            format!("music-server/{} ( {} )", env!("CARGO_PKG_VERSION"), c.trim())
        }
        _ => format!(
            "music-server/{} ( https://github.com/ )",
            env!("CARGO_PKG_VERSION")
        ),
    }
}

/// A minimal async rate limiter: [`acquire`](RateLimiter::acquire) returns only
/// once at least `min_interval` has elapsed since the previous acquire. Cloneable
/// and shareable — every clone draws on the same budget. Because it holds the
/// lock across the wait, it also serializes callers (exactly what a 1 req/s
/// external API wants).
#[derive(Clone)]
pub struct RateLimiter {
    /// Instant of the last granted acquire.
    last: Arc<Mutex<Instant>>,
    min_interval: Duration,
}

impl RateLimiter {
    /// ~1 request/second (1100 ms of slack to stay safely under the limit).
    pub fn per_second() -> Self {
        Self::new(Duration::from_millis(1100))
    }

    pub fn new(min_interval: Duration) -> Self {
        // Seed "one interval ago" so the very first acquire doesn't wait.
        let seed = Instant::now()
            .checked_sub(min_interval)
            .unwrap_or_else(Instant::now);
        Self {
            last: Arc::new(Mutex::new(seed)),
            min_interval,
        }
    }

    /// Wait until the next request is allowed, then reserve this slot.
    pub async fn acquire(&self) {
        let mut last = self.last.lock().await;
        let earliest = *last + self.min_interval;
        let now = Instant::now();
        if earliest > now {
            tokio::time::sleep(earliest - now).await;
        }
        *last = Instant::now();
    }
}
