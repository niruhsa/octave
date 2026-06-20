//! REST fallback client.
//!
//! Mirrors the server's `/auth/*` routes 1:1. Shape matches
//! `server/src/rest/mod.rs`: `POST /auth/login` returns a `LoginResponse`,
//! `GET /auth/whoami` returns a `WhoAmI`, `POST /auth/logout` returns no
//! body. Authentication uses the same `Authorization: Bearer <token>` or
//! `Authorization: SecretKey <key>` header the gRPC interceptor accepts.

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};

use super::{
    Album, Artist, Credential, PermissionTier, Playlist, PlaylistTrack, PlaylistWithTracks,
    ServerConfig, Track,
};
use crate::error::{AppError, AppResult};

pub struct RestClient {
    http: Client,
    base: String,
}

impl RestClient {
    pub fn new(config: &ServerConfig) -> AppResult<Self> {
        let http = Client::builder()
            .use_rustls_tls()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| AppError::Transport(format!("rest build: {e}")))?;
        Ok(Self {
            http,
            base: config.rest_root().to_string(),
        })
    }

    pub async fn login(&self, username: &str, password: &str) -> AppResult<RestLoginOutcome> {
        #[derive(Serialize)]
        struct Body<'a> {
            username: &'a str,
            password: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            token: String,
            user_id: String,
            level: String,
            expires_at: String,
        }

        let url = format!("{}/auth/login", self.base);
        let resp = self
            .http
            .post(url)
            .json(&Body { username, password })
            .send()
            .await
            .map_err(rest_err("login"))?;
        let parsed: Resp = check_status(resp).await?.json().await.map_err(rest_err("login decode"))?;
        Ok(RestLoginOutcome {
            token: parsed.token,
            user_id: parsed.user_id,
            tier: parse_tier(&parsed.level),
            expires_at: parsed.expires_at,
        })
    }

    pub async fn whoami(&self, cred: &Credential) -> AppResult<RestWhoAmI> {
        #[derive(Deserialize)]
        struct Resp {
            kind: String,
            #[serde(default)]
            user_id: String,
            #[serde(default)]
            username: String,
            level: String,
        }

        let url = format!("{}/auth/whoami", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("whoami"))?;
        let parsed: Resp = check_status(resp).await?.json().await.map_err(rest_err("whoami decode"))?;
        Ok(RestWhoAmI {
            kind: parsed.kind,
            user_id: parsed.user_id,
            username: parsed.username,
            tier: parse_tier(&parsed.level),
        })
    }

    pub async fn logout(&self, cred: &Credential) -> AppResult<()> {
        let url = format!("{}/auth/logout", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("logout"))?;
        check_status(resp).await?;
        Ok(())
    }

    /// Cheap liveness check used by online/offline detection.
    pub async fn health(&self) -> AppResult<bool> {
        let url = format!("{}/health", self.base);
        let resp = self.http.get(url).send().await.map_err(rest_err("health"))?;
        Ok(resp.status().is_success())
    }

    // ----- Library reads -------------------------------------------------

    pub async fn list_artists(
        &self,
        cred: &Credential,
        limit: i64,
        offset: i64,
    ) -> AppResult<(Vec<Artist>, i64)> {
        #[derive(Deserialize)]
        struct Resp {
            artists: Vec<ArtistJson>,
            total: i64,
        }
        let url = format!("{}/artists", self.base);
        let resp = self
            .http
            .get(url)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())])
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_artists"))?;
        let body: Resp = check_status(resp).await?.json().await.map_err(rest_err("list_artists decode"))?;
        Ok((body.artists.into_iter().map(Artist::from).collect(), body.total))
    }

    pub async fn search_artists(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Artist>> {
        let url = format!("{}/artists/search", self.base);
        let resp = self
            .http
            .get(url)
            .query(&[
                ("q", query.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ])
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("search_artists"))?;
        let body: Vec<ArtistJson> = check_status(resp).await?.json().await.map_err(rest_err("search_artists decode"))?;
        Ok(body.into_iter().map(Artist::from).collect())
    }

    pub async fn list_albums_by_artist(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<Album>> {
        let url = format!("{}/artists/{artist_id}/albums", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_albums_by_artist"))?;
        let body: Vec<AlbumJson> = check_status(resp).await?.json().await.map_err(rest_err("list_albums_by_artist decode"))?;
        Ok(body.into_iter().map(Album::from).collect())
    }

    pub async fn search_albums(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Album>> {
        let url = format!("{}/albums/search", self.base);
        let resp = self
            .http
            .get(url)
            .query(&[
                ("q", query.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ])
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("search_albums"))?;
        let body: Vec<AlbumJson> = check_status(resp).await?.json().await.map_err(rest_err("search_albums decode"))?;
        Ok(body.into_iter().map(Album::from).collect())
    }

    pub async fn list_tracks_by_album(
        &self,
        cred: &Credential,
        album_id: &str,
    ) -> AppResult<Vec<Track>> {
        let url = format!("{}/albums/{album_id}/tracks", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_tracks_by_album"))?;
        let body: Vec<TrackJson> = check_status(resp).await?.json().await.map_err(rest_err("list_tracks_by_album decode"))?;
        Ok(body.into_iter().map(Track::from).collect())
    }

    pub async fn search_tracks(
        &self,
        cred: &Credential,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> AppResult<Vec<Track>> {
        let url = format!("{}/tracks/search", self.base);
        let resp = self
            .http
            .get(url)
            .query(&[
                ("q", query.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ])
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("search_tracks"))?;
        let body: Vec<TrackJson> = check_status(resp).await?.json().await.map_err(rest_err("search_tracks decode"))?;
        Ok(body.into_iter().map(Track::from).collect())
    }

    // ----- Get-by-id (sync reconcile) ------------------------------------
    //
    // 404 → `Ok(None)` so the sync engine treats a missing server row as
    // "prune locally".

    pub async fn get_artist(&self, cred: &Credential, id: &str) -> AppResult<Option<Artist>> {
        let url = format!("{}/artists/{id}", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("get_artist"))?;
        match opt_status(resp).await? {
            Some(r) => Ok(Some(r.json::<ArtistJson>().await.map_err(rest_err("get_artist decode"))?.into())),
            None => Ok(None),
        }
    }

    pub async fn get_album(&self, cred: &Credential, id: &str) -> AppResult<Option<Album>> {
        let url = format!("{}/albums/{id}", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("get_album"))?;
        match opt_status(resp).await? {
            Some(r) => Ok(Some(r.json::<AlbumJson>().await.map_err(rest_err("get_album decode"))?.into())),
            None => Ok(None),
        }
    }

    pub async fn get_track(&self, cred: &Credential, id: &str) -> AppResult<Option<Track>> {
        let url = format!("{}/tracks/{id}", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("get_track"))?;
        match opt_status(resp).await? {
            Some(r) => Ok(Some(r.json::<TrackJson>().await.map_err(rest_err("get_track decode"))?.into())),
            None => Ok(None),
        }
    }

    // ----- Playlists (sync pull + push) ----------------------------------

    pub async fn list_my_playlists(&self, cred: &Credential) -> AppResult<Vec<Playlist>> {
        let url = format!("{}/playlists", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("list_my_playlists"))?;
        #[derive(Deserialize)]
        struct Resp { playlists: Vec<PlaylistJson> }
        let body: Resp = check_status(resp).await?.json().await.map_err(rest_err("list_my_playlists decode"))?;
        Ok(body.playlists.into_iter().map(Playlist::from).collect())
    }

    pub async fn get_playlist(&self, cred: &Credential, id: &str) -> AppResult<Option<PlaylistWithTracks>> {
        let url = format!("{}/playlists/{id}", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("get_playlist"))?;
        #[derive(Deserialize)]
        struct Resp { playlist: PlaylistJson, tracks: Vec<PlaylistTrackJson> }
        match opt_status(resp).await? {
            Some(r) => {
                let v: Resp = r.json().await.map_err(rest_err("get_playlist decode"))?;
                Ok(Some(PlaylistWithTracks {
                    playlist: v.playlist.into(),
                    tracks: v.tracks.into_iter().map(PlaylistTrack::from).collect(),
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn create_playlist(&self, cred: &Credential, name: &str) -> AppResult<Playlist> {
        let url = format!("{}/playlists", self.base);
        let resp = self.http.post(url).header("authorization", auth_header(cred)).json(&serde_json::json!({ "name": name })).send().await.map_err(rest_err("create_playlist"))?;
        let p: PlaylistJson = check_status(resp).await?.json().await.map_err(rest_err("create_playlist decode"))?;
        Ok(p.into())
    }

    pub async fn rename_playlist(&self, cred: &Credential, id: &str, name: &str) -> AppResult<Playlist> {
        let url = format!("{}/playlists/{id}", self.base);
        let resp = self.http.put(url).header("authorization", auth_header(cred)).json(&serde_json::json!({ "name": name })).send().await.map_err(rest_err("rename_playlist"))?;
        let p: PlaylistJson = check_status(resp).await?.json().await.map_err(rest_err("rename_playlist decode"))?;
        Ok(p.into())
    }

    pub async fn delete_playlist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/playlists/{id}", self.base);
        let resp = self.http.delete(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("delete_playlist"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn add_playlist_track(&self, cred: &Credential, playlist_id: &str, track_id: &str, position: i32) -> AppResult<()> {
        let url = format!("{}/playlists/{playlist_id}/tracks", self.base);
        // position 0 = append (server treats None/0 the same).
        let body = serde_json::json!({ "track_id": track_id, "position": position });
        let resp = self.http.post(url).header("authorization", auth_header(cred)).json(&body).send().await.map_err(rest_err("add_playlist_track"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn remove_playlist_track(&self, cred: &Credential, playlist_id: &str, position: i32) -> AppResult<()> {
        let url = format!("{}/playlists/{playlist_id}/tracks/{position}", self.base);
        let resp = self.http.delete(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("remove_playlist_track"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn reorder_playlist_track(&self, cred: &Credential, playlist_id: &str, from_position: i32, to_position: i32) -> AppResult<()> {
        let url = format!("{}/playlists/{playlist_id}/tracks/{from_position}", self.base);
        let resp = self.http.put(url).header("authorization", auth_header(cred)).json(&serde_json::json!({ "to": to_position })).send().await.map_err(rest_err("reorder_playlist_track"))?;
        check_status(resp).await?;
        Ok(())
    }
}

// REST DTOs (match server/src/rest/library.rs exactly).

#[derive(Deserialize)]
struct ArtistJson {
    id: String,
    name: String,
    sort_name: Option<String>,
}
impl From<ArtistJson> for Artist {
    fn from(a: ArtistJson) -> Self {
        Self { id: a.id, name: a.name, sort_name: a.sort_name }
    }
}

#[derive(Deserialize)]
struct AlbumJson {
    id: String,
    artist_id: String,
    title: String,
    release_year: Option<i64>,
    cover_path: Option<String>,
}
impl From<AlbumJson> for Album {
    fn from(a: AlbumJson) -> Self {
        Self {
            id: a.id,
            artist_id: a.artist_id,
            title: a.title,
            release_year: a.release_year,
            cover_path: a.cover_path,
        }
    }
}

#[derive(Deserialize)]
struct TrackJson {
    id: String,
    album_id: String,
    artist_id: String,
    title: String,
    track_no: Option<i64>,
    disc_no: Option<i64>,
    duration_ms: i64,
    codec: String,
    bitrate_kbps: Option<i64>,
    file_path: String,
    file_size: Option<i64>,
    metadata_json: String,
}
impl From<TrackJson> for Track {
    fn from(t: TrackJson) -> Self {
        Self {
            id: t.id,
            album_id: t.album_id,
            artist_id: t.artist_id,
            title: t.title,
            track_no: t.track_no,
            disc_no: t.disc_no,
            duration_ms: t.duration_ms,
            codec: t.codec,
            bitrate_kbps: t.bitrate_kbps,
            file_path: t.file_path,
            file_size: t.file_size,
            metadata_json: t.metadata_json,
        }
    }
}

#[derive(Deserialize)]
struct PlaylistJson {
    id: String,
    owner_id: String,
    name: String,
}
impl From<PlaylistJson> for Playlist {
    fn from(p: PlaylistJson) -> Self {
        Self { id: p.id, owner_id: p.owner_id, name: p.name }
    }
}

#[derive(Deserialize)]
struct PlaylistTrackJson {
    playlist_id: String,
    track_id: String,
    position: i32,
}
impl From<PlaylistTrackJson> for PlaylistTrack {
    fn from(t: PlaylistTrackJson) -> Self {
        Self { playlist_id: t.playlist_id, track_id: t.track_id, position: t.position as i64 }
    }
}

pub struct RestLoginOutcome {
    pub token: String,
    pub user_id: String,
    pub tier: PermissionTier,
    pub expires_at: String,
}

pub struct RestWhoAmI {
    pub kind: String,
    pub user_id: String,
    pub username: String,
    pub tier: PermissionTier,
}

fn auth_header(cred: &Credential) -> String {
    match cred {
        Credential::SecretKey(k) => format!("SecretKey {k}"),
        Credential::Bearer(t) => format!("Bearer {t}"),
    }
}

fn parse_tier(s: &str) -> PermissionTier {
    match s.to_ascii_lowercase().as_str() {
        "admin" => PermissionTier::Admin,
        "manager" => PermissionTier::Manager,
        _ => PermissionTier::User,
    }
}

fn rest_err(ctx: &'static str) -> impl Fn(reqwest::Error) -> AppError {
    move |e| AppError::Transport(format!("{ctx}: {e}"))
}

/// Like `check_status` but maps 404 to `Ok(None)` for get-by-id calls.
async fn opt_status(resp: reqwest::Response) -> AppResult<Option<reqwest::Response>> {
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    check_status(resp).await.map(Some)
}

/// Convert a non-2xx response into a structured error. The body may carry
/// a server-side message in JSON or plain text; we attach whichever we get.
async fn check_status(resp: reqwest::Response) -> AppResult<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
    };
    Err(if status == StatusCode::UNAUTHORIZED {
        AppError::Unauthenticated(msg)
    } else if status == StatusCode::FORBIDDEN {
        AppError::Forbidden(msg)
    } else {
        AppError::Transport(msg)
    })
}
