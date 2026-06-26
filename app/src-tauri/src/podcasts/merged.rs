//! Merged podcast view types — what the frontend renders. Combines the
//! server's row with local offline state, mirroring `library::merged`.

use serde::{Deserialize, Serialize};

use crate::cache::model as cache_model;
use crate::transport::{Podcast, PodcastEpisode};

/// A podcast show plus whether the user is subscribed + how many of its
/// episodes are downloaded locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedPodcast {
    pub id: String,
    pub feed_url: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub link: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub itunes_id: Option<i64>,
    pub podcastindex_id: Option<i64>,
    pub auto_download: i32,
    pub last_refreshed_at: Option<String>,
    pub subscribed: bool,
    pub downloaded_count: i64,
}

/// An episode plus its offline state. `downloaded` (the client has the file)
/// is distinct from `server_downloaded` (the server has it cached and will
/// serve its stream endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedEpisode {
    pub id: String,
    pub podcast_id: String,
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub episode_no: Option<i64>,
    pub season_no: Option<i64>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate_kbps: Option<i64>,
    pub file_size: Option<i64>,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    /// Local file when downloaded; `None` for stream-only items.
    pub local_file_path: Option<String>,
    /// The server has the audio on disk (its stream endpoint will serve it).
    pub server_downloaded: bool,
    /// The client has it downloaded for offline use.
    pub downloaded: bool,
}

impl MergedPodcast {
    pub fn from_server(p: Podcast, subscribed: bool, downloaded_count: i64) -> Self {
        Self {
            id: p.id,
            feed_url: p.feed_url,
            title: p.title,
            author: p.author,
            description: p.description,
            image_url: p.image_url,
            link: p.link,
            language: p.language,
            categories: p.categories,
            itunes_id: p.itunes_id,
            podcastindex_id: p.podcastindex_id,
            auto_download: p.auto_download,
            last_refreshed_at: p.last_refreshed_at,
            subscribed,
            downloaded_count,
        }
    }

    pub fn from_cache(p: cache_model::Podcast, downloaded_count: i64) -> Self {
        Self {
            id: p.id,
            feed_url: p.feed_url,
            title: p.title,
            author: p.author,
            description: p.description,
            image_url: p.image_url,
            link: None,
            language: p.language,
            categories: serde_json::from_str(&p.categories).unwrap_or_default(),
            itunes_id: None,
            podcastindex_id: None,
            auto_download: 0,
            last_refreshed_at: None,
            subscribed: p.subscribed != 0,
            downloaded_count,
        }
    }
}

impl MergedEpisode {
    pub fn from_server(e: PodcastEpisode, local_file_path: Option<String>) -> Self {
        let downloaded = local_file_path.is_some();
        let server_downloaded = e.downloaded;
        Self {
            id: e.id,
            podcast_id: e.podcast_id,
            guid: e.guid,
            title: e.title,
            description: e.description,
            enclosure_url: e.enclosure_url,
            enclosure_type: e.enclosure_type,
            episode_no: e.episode_no,
            season_no: e.season_no,
            duration_ms: e.duration_ms,
            codec: e.codec,
            bitrate_kbps: e.bitrate_kbps,
            file_size: e.file_size,
            image_url: e.image_url,
            published_at: e.published_at,
            local_file_path,
            server_downloaded,
            downloaded,
        }
    }

    pub fn from_cache(e: cache_model::PodcastEpisode) -> Self {
        Self {
            id: e.id,
            podcast_id: e.podcast_id,
            guid: e.guid,
            title: e.title,
            description: e.description,
            enclosure_url: e.enclosure_url,
            enclosure_type: None,
            episode_no: e.episode_no,
            season_no: e.season_no,
            duration_ms: e.duration_ms,
            codec: e.codec,
            bitrate_kbps: e.bitrate_kbps,
            file_size: e.file_size,
            image_url: None,
            published_at: e.published_at,
            local_file_path: e.local_file_path,
            // The server-state isn't known offline; the local file is what matters.
            server_downloaded: true,
            downloaded: true,
        }
    }
}
