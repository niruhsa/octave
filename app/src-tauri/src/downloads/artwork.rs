//! Best-effort album-cover fetcher.
//!
//! The server fetches covers from the Cover Art Archive into its own
//! `ARTWORK_PATH` and stamps the album's `cover_path`, but it does **not**
//! expose an endpoint to download those bytes to a client. To populate
//! `album_art.local_cover_path` on the client we therefore replicate the
//! server's resolution flow locally: MusicBrainz release search by
//! artist + album title → Cover Art Archive front-cover pull.
//!
//! This is deliberately best-effort: any failure is logged and the track
//! download still succeeds without a cover. MusicBrainz asks for a
//! descriptive User-Agent and rate-limits to ~1 req/s, which is fine here
//! since covers are fetched one-per-album during downloads.

use std::path::Path;

use reqwest::Client;

use crate::error::AppResult;

/// MusicBrainz wants a real UA + a contact. Hard-coded here so the client
/// doesn't ship a config knob for it.
const USER_AGENT: &str = concat!(
    "octave/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/drwhite/music-server)"
);

/// Try to fetch a front cover for `artist` / `album` into `dest`.
///
/// Returns `Ok(true)` on success, `Ok(false)` when no cover could be
/// resolved (no MBID, no CAA entry, decode failure). Never returns `Err`
/// for "no cover found" — only for transport-level problems the caller
/// might want to retry. In practice the caller treats both as "skip".
pub async fn fetch_cover(http: &Client, artist: &str, album: &str, dest: &Path) -> AppResult<bool> {
    let Some(mbid) = resolve_mbid(http, artist, album).await? else {
        return Ok(false);
    };
    let bytes = pull_caa_front(http, &mbid).await?;
    match bytes {
        Some(b) => {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(dest, &b).await?;
            tracing::info!(mbid, bytes = b.len(), "downloaded cover art");
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Query MusicBrainz for the first release matching `release:album AND
/// artist:artist`. Returns the release MBID on success.
async fn resolve_mbid(http: &Client, artist: &str, album: &str) -> AppResult<Option<String>> {
    let query = format!("release:\"{album}\" AND artist:\"{artist}\"");
    let url = reqwest::Url::parse_with_params(
        "https://musicbrainz.org/ws/2/release",
        &[("query", query.as_str()), ("fmt", "json"), ("limit", "1")],
    )
    .map_err(|e| crate::error::AppError::Transport(format!("mbz url: {e}")))?;

    let resp = http
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("mbz search: {e}")))?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), "mbz search non-2xx; no cover");
        return Ok(None);
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("mbz json: {e}")))?;
    let mbid = body
        .get("releases")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(mbid)
}

/// Pull the front cover from the Cover Art Archive for `mbid`. Returns
/// `None` (not an error) when CAA has no front cover (404).
async fn pull_caa_front(http: &Client, mbid: &str) -> AppResult<Option<Vec<u8>>> {
    let url = format!("https://coverartarchive.org/release/{mbid}/front");
    let resp = http
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("caa fetch: {e}")))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), "caa non-2xx; no cover");
        return Ok(None);
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| crate::error::AppError::Transport(format!("caa body: {e}")))?;
    Ok(Some(bytes.to_vec()))
}
