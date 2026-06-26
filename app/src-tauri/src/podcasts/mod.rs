//! Podcast service: subscribed-show + episode reads, online with cache
//! fallback. The directory search and subscribe/unsubscribe are
//! server-authoritative; downloads live in `crate::downloads`.

pub mod merged;
pub mod service;

pub use merged::{MergedEpisode, MergedPodcast};
pub use service::PodcastService;
