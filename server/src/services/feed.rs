//! RSS/Atom podcast feed parsing.
//!
//! Episodes come from the feed itself (the universal source of truth). We use
//! [`feed_rs`] (RSS 2.0 + Atom + media/itunes namespaces) and extract the
//! podcast-relevant fields, mapping them onto the insert-shape DTOs in
//! [`crate::db::models`].
//!
//! Resilience mirrors [`crate::services::tag::read_tags`]: a malformed *item*
//! (no audio enclosure, no usable guid) is **skipped**, not fatal — we index
//! what we can. Only a feed that won't parse at all is an error.

use time::OffsetDateTime;

use crate::error::{AppError, Result};

/// Show-level metadata + the parsed episodes.
#[derive(Debug, Clone, Default)]
pub struct ParsedFeed {
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    pub categories: Vec<String>,
    pub episodes: Vec<ParsedEpisode>,
}

/// One `<item>` with an audio enclosure.
#[derive(Debug, Clone, Default)]
pub struct ParsedEpisode {
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    /// From the feed (`itunes:duration` / media duration). May be refined to a
    /// measured value when the episode is downloaded.
    pub duration_ms: Option<i64>,
    pub episode_no: Option<i32>,
    pub season_no: Option<i32>,
    /// Per-episode artwork URL, when the feed carries one.
    pub image_url: Option<String>,
    pub published_at: Option<OffsetDateTime>,
}

/// Parse a feed document. Errors only when the bytes aren't a parseable feed;
/// individual items without an audio enclosure are dropped.
pub fn parse_feed(bytes: &[u8]) -> Result<ParsedFeed> {
    let feed = feed_rs::parser::parse(bytes)
        .map_err(|e| AppError::InvalidArgument(format!("parse feed: {e}")))?;

    let title = feed
        .title
        .map(|t| t.content)
        .unwrap_or_else(|| "Untitled Podcast".to_string());
    let author = feed.authors.into_iter().next().map(|p| p.name);
    let description = feed.description.map(|t| t.content);
    let image_url = feed
        .logo
        .or(feed.icon)
        .map(|img| img.uri);
    let link = feed.links.into_iter().next().map(|l| l.href);
    let language = feed.language;
    let categories: Vec<String> = feed
        .categories
        .into_iter()
        .map(|c| c.label.unwrap_or(c.term))
        .filter(|s| !s.trim().is_empty())
        .collect();

    let mut episodes = Vec::new();
    for entry in feed.entries {
        if let Some(ep) = parse_entry(entry) {
            episodes.push(ep);
        }
    }

    Ok(ParsedFeed {
        title,
        author,
        description,
        image_url,
        link,
        language,
        categories,
        episodes,
    })
}

/// Map a feed entry to a `ParsedEpisode`, or `None` when it carries no audio
/// enclosure (not a real episode — e.g. a chapter/blog-only item).
fn parse_entry(entry: feed_rs::model::Entry) -> Option<ParsedEpisode> {
    // Find the first audio enclosure across the entry's media objects.
    let mut enclosure_url: Option<String> = None;
    let mut enclosure_type: Option<String> = None;
    let mut duration_ms: Option<i64> = None;
    let mut image_url: Option<String> = None;

    for media in &entry.media {
        if image_url.is_none() {
            image_url = media.thumbnails.first().map(|t| t.image.uri.clone());
        }
        if duration_ms.is_none() {
            duration_ms = media.duration.map(|d| d.as_millis() as i64);
        }
        for content in &media.content {
            let ctype = content.content_type.as_ref().map(|m| m.to_string());
            let url = content.url.as_ref().map(|u| u.to_string());
            let is_audio = ctype
                .as_deref()
                .map(|c| c.starts_with("audio"))
                .unwrap_or(false)
                || url.as_deref().map(looks_like_audio).unwrap_or(false);
            if is_audio && enclosure_url.is_none() {
                enclosure_url = url;
                enclosure_type = ctype;
                if duration_ms.is_none() {
                    duration_ms = content.duration.map(|d| d.as_millis() as i64);
                }
            }
        }
    }

    let enclosure_url = enclosure_url?; // no audio → not an episode

    // GUID: prefer the feed's own id, fall back to the enclosure URL.
    let guid = if entry.id.trim().is_empty() {
        enclosure_url.clone()
    } else {
        entry.id
    };

    let title = entry
        .title
        .map(|t| t.content)
        .unwrap_or_else(|| "Untitled Episode".to_string());
    let description = entry
        .summary
        .map(|t| t.content)
        .or_else(|| entry.content.and_then(|c| c.body));
    let published_at = entry
        .published
        .and_then(|dt| OffsetDateTime::from_unix_timestamp(dt.timestamp()).ok());

    Some(ParsedEpisode {
        guid,
        title,
        description,
        enclosure_url,
        enclosure_type,
        duration_ms,
        // itunes:episode / itunes:season aren't surfaced by feed-rs in a stable
        // field; left None (best-effort — playback/download/notify don't need them).
        episode_no: None,
        season_no: None,
        image_url,
        published_at,
    })
}

/// Heuristic: does a URL path look like an audio file? Used when the enclosure
/// omits a usable content-type.
fn looks_like_audio(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    [".mp3", ".m4a", ".aac", ".ogg", ".opus", ".flac", ".wav", ".mp4"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = include_bytes!("../../tests/fixtures/feeds/sample.xml");

    #[test]
    fn parses_episodes_and_show_metadata() {
        let feed = parse_feed(SAMPLE).expect("parse");
        assert_eq!(feed.title, "Test Podcast");
        assert_eq!(feed.author.as_deref(), Some("Jane Host"));
        assert!(feed.image_url.is_some());
        // Two <item>s carry audio enclosures; one item has no enclosure and is
        // dropped.
        assert_eq!(feed.episodes.len(), 2);

        let first = &feed.episodes[0];
        assert_eq!(first.guid, "episode-001");
        assert_eq!(first.title, "Episode One");
        assert_eq!(
            first.enclosure_url,
            "https://cdn.example.com/ep1.mp3"
        );
        assert_eq!(first.enclosure_type.as_deref(), Some("audio/mpeg"));
        assert!(first.published_at.is_some());
    }

    #[test]
    fn item_without_enclosure_is_skipped() {
        let feed = parse_feed(SAMPLE).expect("parse");
        assert!(feed.episodes.iter().all(|e| !e.enclosure_url.is_empty()));
        assert!(feed.episodes.iter().all(|e| e.guid != "no-audio-item"));
    }

    #[test]
    fn rejects_non_feed_bytes() {
        let err = parse_feed(b"not a feed at all").unwrap_err();
        assert!(matches!(err, AppError::InvalidArgument(_)));
    }
}
