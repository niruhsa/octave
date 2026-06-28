//! Merged podcast view types — what the frontend renders. Combines the
//! server's row with local offline state, mirroring `library::merged`.

use serde::{Deserialize, Serialize};

use crate::cache::model as cache_model;
use crate::transport::Podcast;

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
    /// Sum of the on-disk bytes of every downloaded episode of this show (server-side).
    #[serde(default)]
    pub storage_bytes: i64,
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
    /// Last playback position in ms (0 = not started). Drives "resume".
    #[serde(default)]
    pub position_ms: i64,
    /// Played to (near) the end — shown as "listened".
    #[serde(default)]
    pub completed: bool,
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
            storage_bytes: p.storage_bytes,
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
            storage_bytes: p.storage_bytes,
        }
    }
}

impl MergedEpisode {
    /// A cached row with no fresh server signal — `server_downloaded` defaults
    /// to `false` (play direct from the origin enclosure, which always works).
    /// Used for the offline list and for episodes not re-fetched this sync.
    pub fn from_cache(e: cache_model::PodcastEpisode) -> Self {
        Self::from_cache_row(e, false)
    }

    /// A cached row plus the server's freshly-synced "has it cached" flag, so
    /// the newest episodes route playback through our server while older cached
    /// ones fall back to the origin. `downloaded` is the presence of the local
    /// file — a metadata-only row (`local_file_path` NULL) is not downloaded.
    pub fn from_cache_row(e: cache_model::PodcastEpisode, server_downloaded: bool) -> Self {
        let downloaded = e.local_file_path.is_some();
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
            server_downloaded,
            downloaded,
            position_ms: 0,
            completed: false,
        }
    }
}
