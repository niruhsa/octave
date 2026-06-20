//! Album artwork fetch + cache.
//!
//! Looks up a release on MusicBrainz by artist + album, then pulls the
//! front-cover image from the Cover Art Archive (CAA) and caches it on disk
//! under `ARTWORK_PATH`.  The album row's `cover_path` is then updated via
//! the [`LibraryService`] so the change is audited like any other mutation.
//!
//! External calls are isolated behind the [`CoverArtSource`] trait so the
//! service is unit-testable without network access.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::PermissionLevel;
use crate::error::{AppError, Result};
use crate::services::library::LibraryService;

/// User-Agent required by both MusicBrainz and the Cover Art Archive.
const USER_AGENT: &str = concat!(
    "music-server/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/ )"
);

/// A fetched cover image: its bytes and the source content-type.
#[derive(Debug, Clone)]
pub struct CoverImage {
    pub bytes: Vec<u8>,
    /// e.g. `image/jpeg`, `image/png`.
    pub content_type: String,
}

impl CoverImage {
    /// File extension derived from the content type (`jpg` fallback).
    pub fn ext(&self) -> &str {
        match self.content_type.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        }
    }
}

/// Abstraction over the external cover-art lookup so the service can be
/// tested against a fake.
#[async_trait]
pub trait CoverArtSource: Send + Sync {
    /// Fetch the front-cover image for `artist` + `album`, or `Ok(None)`
    /// when no release / cover is found.
    async fn fetch_cover(&self, artist: &str, album: &str) -> Result<Option<CoverImage>>;
}

/// Cover Art Archive source backed by MusicBrainz release search.
pub struct CoverArtArchive {
    client: reqwest::Client,
}

impl Default for CoverArtArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl CoverArtArchive {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .expect("reqwest client build");
        Self { client }
    }
}

#[async_trait]
impl CoverArtSource for CoverArtArchive {
    async fn fetch_cover(&self, artist: &str, album: &str) -> Result<Option<CoverImage>> {
        // 1. Find a release MBID via MusicBrainz search.
        let query = format!("release:\"{album}\" AND artist:\"{artist}\"");
        let search_url = "https://musicbrainz.org/ws/2/release";
        let resp = self
            .client
            .get(search_url)
            .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "1")])
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("musicbrainz search: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "musicbrainz search status {}",
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("musicbrainz json: {e}")))?;
        let mbid = body
            .get("releases")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|rel| rel.get("id"))
            .and_then(|id| id.as_str());
        let mbid = match mbid {
            Some(id) => id.to_string(),
            None => {
                debug!(artist, album, "artwork: no MusicBrainz release found");
                return Ok(None);
            }
        };

        // 2. Pull the front cover from the Cover Art Archive.
        let caa_url = format!("https://coverartarchive.org/release/{mbid}/front");
        let resp = self
            .client
            .get(&caa_url)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("cover art archive: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            debug!(mbid, "artwork: no front cover in CAA");
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(AppError::Internal(format!(
                "cover art archive status {}",
                resp.status()
            )));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AppError::Internal(format!("cover art bytes: {e}")))?
            .to_vec();
        Ok(Some(CoverImage {
            bytes,
            content_type,
        }))
    }
}

/// Orchestrates artwork fetch → cache → album `cover_path` update.
#[derive(Clone)]
pub struct ArtworkService {
    pub library: LibraryService,
    pub source: Arc<dyn CoverArtSource>,
    /// Directory where fetched covers are cached. `None` disables caching to
    /// disk (the album row is still updated if a path can be derived — but
    /// without a cache dir there is nowhere to write, so fetch is a no-op).
    pub cache_dir: Option<PathBuf>,
}

impl ArtworkService {
    pub fn new(
        library: LibraryService,
        source: Arc<dyn CoverArtSource>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            library,
            source,
            cache_dir,
        }
    }

    /// Fetch artwork for `album_id`, cache it on disk, and update the album's
    /// `cover_path` (audited). Manager+ only.
    ///
    /// Returns the cached `cover_path` on success, `Ok(None)` when no cover
    /// was found upstream.
    pub async fn fetch_for_album(
        &self,
        caller: &Identity,
        album_id: Uuid,
    ) -> Result<Option<String>> {
        caller.require(PermissionLevel::Manager)?;

        let cache_dir = self.cache_dir.as_ref().ok_or_else(|| {
            AppError::Config("artwork cache dir not configured (set ARTWORK_PATH)".into())
        })?;

        let album = self.library.get_album(caller, album_id).await?;
        let artist = self.library.get_artist(caller, album.artist_id).await?;

        let cover = match self.source.fetch_cover(&artist.name, &album.title).await? {
            Some(c) => c,
            None => return Ok(None),
        };

        // Cache to <cache_dir>/<album_id>.<ext>.
        tokio::fs::create_dir_all(cache_dir)
            .await
            .map_err(AppError::Io)?;
        let file_name = format!("{album_id}.{}", cover.ext());
        let dest = cache_dir.join(&file_name);
        tokio::fs::write(&dest, &cover.bytes)
            .await
            .map_err(AppError::Io)?;
        let cover_path = dest.to_string_lossy().into_owned();

        // Update the album row (audited via LibraryService).
        self.library
            .update_album(
                caller,
                album_id,
                &album.title,
                album.release_year,
                Some(&cover_path),
            )
            .await?;

        debug!(%album_id, cover_path, "artwork: cached + album updated");
        Ok(Some(cover_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_ext_mapping() {
        let mk = |ct: &str| CoverImage {
            bytes: vec![],
            content_type: ct.into(),
        };
        assert_eq!(mk("image/jpeg").ext(), "jpg");
        assert_eq!(mk("image/png").ext(), "png");
        assert_eq!(mk("image/webp").ext(), "webp");
        assert_eq!(mk("image/gif").ext(), "gif");
        assert_eq!(mk("application/octet-stream").ext(), "jpg");
    }
}
