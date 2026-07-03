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
    Album, AlbumFolderInfo, ArchiveUploadResult, Artist, ArtistStat, ArtistStoragePaths, ChunkAck,
    Credential,
    DiscoverSection,
    EpisodeProgress, FingerprintStatus, LibraryStorage, ListeningStats, MetadataEdit, PermissionTier,
    PlayEvent, RelocateReport,
    PlayHistoryPage, PlayInput, Playlist, PlaylistTrack, PlaylistWithTracks, Podcast,
    PodcastCandidate, PodcastEpisode, RefreshReport, RescanReport, ServerConfig, SingleUploadResult,
    Track, TrackStat, UploadEvent, UploadInitRequest, UploadListFilter, UploadResult, UploadSummary,
    UploadView,
};
use crate::error::{AppError, AppResult};
use super::{
    DiscographyCandidate, DiscographyIgnore, DiscographyReport, DiscographyStatus,
    DiscographySyncAll, DiscographySyncResult,
};

pub struct RestClient {
    http: Client,
    base: String,
}

impl RestClient {
    pub fn new(config: &ServerConfig) -> AppResult<Self> {
        // rustls over `https`, trusting both the bundled webpki roots and the
        // OS native trust store (the `rustls-tls-native-roots` feature, on by
        // default once enabled). This mirrors the gRPC client's
        // `with_enabled_roots()` so a cert that works for gRPC also works here.
        let http = Client::builder()
            .use_rustls_tls()
            .user_agent(crate::USER_AGENT)
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

    /// Register a new account. Server-gated to Admin callers. `level` is
    /// sent as the lowercase tier string the server's `PermissionLevel`
    /// serde expects ("user" / "manager" / "admin"). Returns the new user id.
    pub async fn register(
        &self,
        cred: &Credential,
        username: &str,
        password: &str,
        level: super::PermissionTier,
    ) -> AppResult<String> {
        #[derive(Serialize)]
        struct Body<'a> {
            username: &'a str,
            password: &'a str,
            level: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            user_id: String,
        }
        let level_str = tier_to_rest_str(level);
        let url = format!("{}/auth/register", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&Body {
                username,
                password,
                level: level_str,
            })
            .send()
            .await
            .map_err(rest_err("register"))?;
        // 400 (bad username / short password / duplicate) currently maps to
        // `Transport(msg)` via `check_status`; the message still surfaces to
        // the UI, which is all the user needs. 403 → Forbidden.
        let parsed: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("register decode"))?;
        Ok(parsed.user_id)
    }

    /// Change (or admin-reset) a user's password via `PUT /users/:id/password`.
    /// `old_password` is empty for admin/secret-key resets; required +
    /// verified server-side for non-admin self-changes.
    pub async fn change_password(
        &self,
        cred: &Credential,
        target_user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> AppResult<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            old_password: &'a str,
            new_password: &'a str,
        }
        let url = format!("{}/users/{target_user_id}/password", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&Body {
                old_password,
                new_password,
            })
            .send()
            .await
            .map_err(rest_err("change_password"))?;
        check_status(resp).await?;
        Ok(())
    }

    /// List every registered user. Admin-gated server-side; the `GET /users`
    /// endpoint returns `{ users: [{id, username, level}] }`.
    pub async fn list_users(&self, cred: &Credential) -> AppResult<Vec<super::UserEntry>> {
        #[derive(Deserialize)]
        struct Resp {
            users: Vec<UserJson>,
        }
        #[derive(Deserialize)]
        struct UserJson {
            id: String,
            username: String,
            level: String,
        }
        let url = format!("{}/users", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_users"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_users decode"))?;
        Ok(body
            .users
            .into_iter()
            .map(|u| super::UserEntry {
                id: u.id,
                username: u.username,
                level: parse_tier(&u.level),
            })
            .collect())
    }

    /// Delete a user account via `DELETE /users/:id`. Admin-gated server-side.
    pub async fn delete_user(&self, cred: &Credential, user_id: &str) -> AppResult<()> {
        let url = format!("{}/users/{user_id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("delete_user"))?;
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
        // Server wraps this list as `{ albums, total }` (the `total` field is
        // unused here, so we don't declare it). Decoding a bare array here
        // silently failed → cache fallback → "no albums".
        #[derive(Deserialize)]
        struct Resp {
            albums: Vec<AlbumJson>,
        }
        let body: Resp = check_status(resp).await?.json().await.map_err(rest_err("list_albums_by_artist decode"))?;
        Ok(body.albums.into_iter().map(Album::from).collect())
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
        // Server wraps this list as `{ tracks, total }`; decoding a bare array
        // silently failed → cache fallback → "no tracks".
        #[derive(Deserialize)]
        struct Resp {
            tracks: Vec<TrackJson>,
        }
        let body: Resp = check_status(resp).await?.json().await.map_err(rest_err("list_tracks_by_album decode"))?;
        Ok(body.tracks.into_iter().map(Track::from).collect())
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

    pub async fn get_library_storage(&self, cred: &Credential) -> AppResult<LibraryStorage> {
        let url = format!("{}/library/storage", self.base);
        let resp = self.http.get(url).header("authorization", auth_header(cred)).send().await.map_err(rest_err("get_library_storage"))?;
        let r = check_status(resp).await?;
        Ok(r.json::<LibraryStorageJson>().await.map_err(rest_err("get_library_storage decode"))?.into())
    }

    // ----- Delete (Manager+ gated server-side) ----------------------------

    pub async fn delete_artist(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/artists/{id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("delete_artist"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn delete_album(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/albums/{id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("delete_album"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn delete_track(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/tracks/{id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("delete_track"))?;
        check_status(resp).await?;
        Ok(())
    }

    // ----- Metadata edit (Phase 9; Manager+ gated server-side) -------------

    /// `PATCH /tracks/:id/metadata` — `MetadataEdit` serialises only the
    /// touched fields (others are omitted → "leave unchanged" server-side).
    pub async fn edit_track_metadata(
        &self,
        cred: &Credential,
        id: &str,
        edit: &MetadataEdit,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{id}/metadata", self.base);
        let resp = self
            .http
            .patch(url)
            .header("authorization", auth_header(cred))
            .json(edit)
            .send()
            .await
            .map_err(rest_err("edit_track_metadata"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("edit_track_metadata decode"))?;
        Ok(body.into())
    }

    // ----- Merge + aliases (Phase 10; Manager+ gated server-side) ----------

    pub async fn merge_artists(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Artist> {
        let url = format!("{}/artists/{survivor_id}/merge", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "duplicate_id": duplicate_id }))
            .send()
            .await
            .map_err(rest_err("merge_artists"))?;
        let body: ArtistJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("merge_artists decode"))?;
        Ok(body.into())
    }

    pub async fn list_artist_library_paths(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<ArtistStoragePaths> {
        let url = format!("{}/artists/{artist_id}/library-paths", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_artist_library_paths"))?;
        check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_artist_library_paths decode"))
    }

    pub async fn set_artist_language(
        &self,
        cred: &Credential,
        artist_id: &str,
        target_language: &str,
        target_folder: Option<&str>,
    ) -> AppResult<RelocateReport> {
        let url = format!("{}/artists/{artist_id}/library-language", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({
                "target_language": target_language,
                "target_folder": target_folder,
            }))
            .send()
            .await
            .map_err(rest_err("set_artist_language"))?;
        check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_artist_language decode"))
    }

    pub async fn album_folder(
        &self,
        cred: &Credential,
        album_id: &str,
    ) -> AppResult<AlbumFolderInfo> {
        let url = format!("{}/albums/{album_id}/folder", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("album_folder"))?;
        check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("album_folder decode"))
    }

    pub async fn rename_album_folder(
        &self,
        cred: &Credential,
        album_id: &str,
        folder_name: Option<&str>,
    ) -> AppResult<RelocateReport> {
        let url = format!("{}/albums/{album_id}/folder", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "folder_name": folder_name }))
            .send()
            .await
            .map_err(rest_err("rename_album_folder"))?;
        check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("rename_album_folder decode"))
    }

    pub async fn merge_albums(
        &self,
        cred: &Credential,
        survivor_id: &str,
        duplicate_id: &str,
    ) -> AppResult<Album> {
        let url = format!("{}/albums/{survivor_id}/merge", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "duplicate_id": duplicate_id }))
            .send()
            .await
            .map_err(rest_err("merge_albums"))?;
        let body: AlbumJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("merge_albums decode"))?;
        Ok(body.into())
    }

    pub async fn move_track(
        &self,
        cred: &Credential,
        track_id: &str,
        album_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/move", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "album_id": album_id, "single_release": single_release }))
            .send()
            .await
            .map_err(rest_err("move_track"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("move_track decode"))?;
        Ok(body.into())
    }

    pub async fn set_track_single_release(
        &self,
        cred: &Credential,
        track_id: &str,
        single_release: bool,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/single-release", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "single_release": single_release }))
            .send()
            .await
            .map_err(rest_err("set_track_single_release"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_track_single_release decode"))?;
        Ok(body.into())
    }

    pub async fn set_track_explicit(
        &self,
        cred: &Credential,
        track_id: &str,
        explicit: bool,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/explicit", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "explicit": explicit }))
            .send()
            .await
            .map_err(rest_err("set_track_explicit"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_track_explicit decode"))?;
        Ok(body.into())
    }

    pub async fn set_album_type(
        &self,
        cred: &Credential,
        album_id: &str,
        album_type: &str,
        single_track_id: Option<&str>,
    ) -> AppResult<Album> {
        let url = format!("{}/albums/{album_id}/type", self.base);
        let mut body = serde_json::json!({ "album_type": album_type });
        if let Some(tid) = single_track_id {
            body["single_track_id"] = serde_json::json!(tid);
        }
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&body)
            .send()
            .await
            .map_err(rest_err("set_album_type"))?;
        let body: AlbumJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_album_type decode"))?;
        Ok(body.into())
    }

    pub async fn add_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        name: &str,
        sort_name: Option<&str>,
        language: Option<&str>,
    ) -> AppResult<Artist> {
        let url = format!("{}/artists/{artist_id}/aliases", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({
                "name": name,
                "sort_name": sort_name,
                "language": language,
            }))
            .send()
            .await
            .map_err(rest_err("add_artist_alias"))?;
        let body: ArtistJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("add_artist_alias decode"))?;
        Ok(body.into())
    }

    pub async fn remove_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        let url = format!("{}/artists/{artist_id}/aliases/{alias_id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("remove_artist_alias"))?;
        let body: ArtistJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("remove_artist_alias decode"))?;
        Ok(body.into())
    }

    pub async fn set_primary_artist_alias(
        &self,
        cred: &Credential,
        artist_id: &str,
        alias_id: &str,
    ) -> AppResult<Artist> {
        let url = format!("{}/artists/{artist_id}/primary-alias", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "alias_id": alias_id }))
            .send()
            .await
            .map_err(rest_err("set_primary_artist_alias"))?;
        let body: ArtistJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_primary_artist_alias decode"))?;
        Ok(body.into())
    }

    pub async fn add_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<Album> {
        let url = format!("{}/albums/{album_id}/aliases", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "title": title, "language": language }))
            .send()
            .await
            .map_err(rest_err("add_album_alias"))?;
        let body: AlbumJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("add_album_alias decode"))?;
        Ok(body.into())
    }

    pub async fn remove_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        let url = format!("{}/albums/{album_id}/aliases/{alias_id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("remove_album_alias"))?;
        let body: AlbumJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("remove_album_alias decode"))?;
        Ok(body.into())
    }

    pub async fn set_primary_album_alias(
        &self,
        cred: &Credential,
        album_id: &str,
        alias_id: &str,
    ) -> AppResult<Album> {
        let url = format!("{}/albums/{album_id}/primary-alias", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "alias_id": alias_id }))
            .send()
            .await
            .map_err(rest_err("set_primary_album_alias"))?;
        let body: AlbumJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_primary_album_alias decode"))?;
        Ok(body.into())
    }

    pub async fn list_track_aliases(
        &self,
        cred: &Credential,
        track_id: &str,
    ) -> AppResult<Vec<super::AliasInfo>> {
        let url = format!("{}/tracks/{track_id}/aliases", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_track_aliases"))?;
        let body: Vec<AliasJson> = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_track_aliases decode"))?;
        Ok(body.into_iter().map(Into::into).collect())
    }

    pub async fn add_track_alias(
        &self,
        cred: &Credential,
        track_id: &str,
        title: &str,
        language: Option<&str>,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/aliases", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "title": title, "language": language }))
            .send()
            .await
            .map_err(rest_err("add_track_alias"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("add_track_alias decode"))?;
        Ok(body.into())
    }

    pub async fn remove_track_alias(
        &self,
        cred: &Credential,
        track_id: &str,
        alias_id: &str,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/aliases/{alias_id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("remove_track_alias"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("remove_track_alias decode"))?;
        Ok(body.into())
    }

    pub async fn set_primary_track_alias(
        &self,
        cred: &Credential,
        track_id: &str,
        alias_id: &str,
    ) -> AppResult<Track> {
        let url = format!("{}/tracks/{track_id}/primary-alias", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "alias_id": alias_id }))
            .send()
            .await
            .map_err(rest_err("set_primary_track_alias"))?;
        let body: TrackJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_primary_track_alias decode"))?;
        Ok(body.into())
    }

    // ----- Follows & notifications (Phase 10) ----------------------------

    pub async fn follow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        let url = format!("{}/artists/{artist_id}/follow", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("follow_artist"))?;
        check_status(resp).await?;
        Ok(true)
    }

    pub async fn unfollow_artist(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        let url = format!("{}/artists/{artist_id}/follow", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("unfollow_artist"))?;
        check_status(resp).await?;
        Ok(false)
    }

    pub async fn is_following(&self, cred: &Credential, artist_id: &str) -> AppResult<bool> {
        #[derive(Deserialize)]
        struct Resp {
            following: bool,
        }
        let url = format!("{}/artists/{artist_id}/follow", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("is_following"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("is_following decode"))?;
        Ok(body.following)
    }

    pub async fn list_following(&self, cred: &Credential) -> AppResult<Vec<Artist>> {
        #[derive(Deserialize)]
        struct Resp {
            artists: Vec<ArtistJson>,
        }
        let url = format!("{}/following", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_following"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_following decode"))?;
        Ok(body.artists.into_iter().map(Into::into).collect())
    }

    pub async fn list_notifications(
        &self,
        cred: &Credential,
        unread_only: bool,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<super::NotificationPage> {
        let mut url = format!("{}/notifications?unread={unread_only}", self.base);
        if let Some(l) = limit {
            url.push_str(&format!("&limit={l}"));
        }
        if let Some(o) = offset {
            url.push_str(&format!("&offset={o}"));
        }
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_notifications"))?;
        let body: NotificationPageJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_notifications decode"))?;
        Ok(body.into())
    }

    pub async fn notifications_unread_count(&self, cred: &Credential) -> AppResult<i64> {
        #[derive(Deserialize)]
        struct Resp {
            unread_count: i64,
        }
        let url = format!("{}/notifications/unread-count", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("notifications_unread_count"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("notifications_unread_count decode"))?;
        Ok(body.unread_count)
    }

    pub async fn mark_notification_read(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/notifications/mark-read", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "id": id }))
            .send()
            .await
            .map_err(rest_err("mark_notification_read"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn mark_all_notifications_read(&self, cred: &Credential) -> AppResult<u64> {
        #[derive(Deserialize)]
        struct Resp {
            marked: u64,
        }
        let url = format!("{}/notifications/mark-all-read", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("mark_all_notifications_read"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("mark_all_notifications_read decode"))?;
        Ok(body.marked)
    }

    pub async fn register_device(
        &self,
        cred: &Credential,
        token: &str,
        platform: &str,
    ) -> AppResult<()> {
        let url = format!("{}/devices", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "token": token, "platform": platform }))
            .send()
            .await
            .map_err(rest_err("register_device"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn unregister_device(&self, cred: &Credential, token: &str) -> AppResult<()> {
        let url = format!("{}/devices", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "token": token }))
            .send()
            .await
            .map_err(rest_err("unregister_device"))?;
        check_status(resp).await?;
        Ok(())
    }

    // ----- Play history (Phase 11) ---------------------------------------

    pub async fn record_plays(&self, cred: &Credential, events: &[PlayInput]) -> AppResult<u64> {
        #[derive(Deserialize)]
        struct Resp {
            recorded: u64,
        }
        let url = format!("{}/history", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "events": events }))
            .send()
            .await
            .map_err(rest_err("record_plays"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("record_plays decode"))?;
        Ok(body.recorded)
    }

    pub async fn list_play_history(
        &self,
        cred: &Credential,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> AppResult<PlayHistoryPage> {
        let mut url = format!("{}/history?", self.base);
        if let Some(l) = limit {
            url.push_str(&format!("limit={l}&"));
        }
        if let Some(o) = offset {
            url.push_str(&format!("offset={o}"));
        }
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_play_history"))?;
        let body: PlayHistoryPageJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_play_history decode"))?;
        Ok(body.into())
    }

    pub async fn play_stats(
        &self,
        cred: &Credential,
        window_days: Option<i64>,
        limit: Option<i64>,
    ) -> AppResult<ListeningStats> {
        let mut url = format!("{}/history/stats?", self.base);
        if let Some(w) = window_days {
            url.push_str(&format!("window_days={w}&"));
        }
        if let Some(l) = limit {
            url.push_str(&format!("limit={l}"));
        }
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("play_stats"))?;
        let body: StatsJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("play_stats decode"))?;
        Ok(body.into())
    }

    // ----- Favorites (Phase 11) ------------------------------------------

    pub async fn favorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        let seg = fav_path_segment(kind)?;
        let url = format!("{}/{seg}/{entity_id}/favorite", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("favorite"))?;
        check_status(resp).await?;
        Ok(true)
    }

    pub async fn unfavorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        let seg = fav_path_segment(kind)?;
        let url = format!("{}/{seg}/{entity_id}/favorite", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("unfavorite"))?;
        check_status(resp).await?;
        Ok(false)
    }

    pub async fn is_favorite(&self, cred: &Credential, kind: &str, entity_id: &str) -> AppResult<bool> {
        #[derive(Deserialize)]
        struct Resp {
            favorited: bool,
        }
        let seg = fav_path_segment(kind)?;
        let url = format!("{}/{seg}/{entity_id}/favorite", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("is_favorite"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("is_favorite decode"))?;
        Ok(body.favorited)
    }

    pub async fn list_favorite_tracks(&self, cred: &Credential) -> AppResult<Vec<Track>> {
        #[derive(Deserialize)]
        struct Resp {
            tracks: Vec<TrackJson>,
        }
        let url = format!("{}/favorites/tracks", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_favorite_tracks"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_favorite_tracks decode"))?;
        Ok(body.tracks.into_iter().map(Into::into).collect())
    }

    pub async fn list_favorite_albums(&self, cred: &Credential) -> AppResult<Vec<Album>> {
        #[derive(Deserialize)]
        struct Resp {
            albums: Vec<AlbumJson>,
        }
        let url = format!("{}/favorites/albums", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_favorite_albums"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_favorite_albums decode"))?;
        Ok(body.albums.into_iter().map(Into::into).collect())
    }

    pub async fn list_favorite_artists(&self, cred: &Credential) -> AppResult<Vec<Artist>> {
        #[derive(Deserialize)]
        struct Resp {
            artists: Vec<ArtistJson>,
        }
        let url = format!("{}/favorites/artists", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_favorite_artists"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_favorite_artists decode"))?;
        Ok(body.artists.into_iter().map(Into::into).collect())
    }

    pub async fn favorited_track_ids(&self, cred: &Credential) -> AppResult<Vec<String>> {
        #[derive(Deserialize)]
        struct Resp {
            track_ids: Vec<String>,
        }
        let url = format!("{}/favorites/track-ids", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("favorited_track_ids"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("favorited_track_ids decode"))?;
        Ok(body.track_ids)
    }

    // ----- Discover (Phase 11) -------------------------------------------

    pub async fn discover_home(&self, cred: &Credential) -> AppResult<Vec<DiscoverSection>> {
        #[derive(Deserialize)]
        struct SectionJson {
            id: String,
            title: String,
            #[serde(default)]
            albums: Vec<AlbumJson>,
        }
        #[derive(Deserialize)]
        struct Resp {
            sections: Vec<SectionJson>,
        }
        let url = format!("{}/discover", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discover_home"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discover_home decode"))?;
        Ok(body
            .sections
            .into_iter()
            .map(|s| DiscoverSection {
                id: s.id,
                title: s.title,
                albums: s.albums.into_iter().map(Into::into).collect(),
            })
            .collect())
    }

    pub async fn discover_radio(
        &self,
        cred: &Credential,
        seed_artist_id: Option<&str>,
        seed_album_id: Option<&str>,
        seed_track_id: Option<&str>,
    ) -> AppResult<Vec<Track>> {
        #[derive(Deserialize)]
        struct Resp {
            tracks: Vec<TrackJson>,
        }
        let mut url = format!("{}/discover/radio?", self.base);
        if let Some(a) = seed_artist_id {
            url.push_str(&format!("seed_artist_id={a}&"));
        }
        if let Some(a) = seed_album_id {
            url.push_str(&format!("seed_album_id={a}&"));
        }
        if let Some(t) = seed_track_id {
            url.push_str(&format!("seed_track_id={t}"));
        }
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discover_radio"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discover_radio decode"))?;
        Ok(body.tracks.into_iter().map(Into::into).collect())
    }

    /// Acoustic "sounds like this" — the seed track's nearest neighbors (Phase 12).
    pub async fn discover_similar(
        &self,
        cred: &Credential,
        track_id: &str,
        limit: i32,
    ) -> AppResult<Vec<Track>> {
        #[derive(Deserialize)]
        struct Resp {
            tracks: Vec<TrackJson>,
        }
        let url = format!("{}/tracks/{track_id}/similar?limit={limit}", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discover_similar"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discover_similar decode"))?;
        Ok(body.tracks.into_iter().map(Into::into).collect())
    }

    /// Spotify-style playlist recommendations — tracks similar to the whole
    /// playlist (aggregated over `seed_track_ids`), excluding the seeds (Phase 12).
    pub async fn discover_playlist_recommendations(
        &self,
        cred: &Credential,
        seed_track_ids: &[String],
        limit: i32,
    ) -> AppResult<Vec<Track>> {
        #[derive(Serialize)]
        struct Body<'a> {
            seed_track_ids: &'a [String],
            limit: i32,
        }
        #[derive(Deserialize)]
        struct Resp {
            tracks: Vec<TrackJson>,
        }
        let url = format!("{}/discover/recommendations", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&Body { seed_track_ids, limit })
            .send()
            .await
            .map_err(rest_err("discover_playlist_recommendations"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discover_playlist_recommendations decode"))?;
        Ok(body.tracks.into_iter().map(Into::into).collect())
    }

    /// Fingerprint analysis coverage (Phase 12).
    pub async fn fingerprint_status(&self, cred: &Credential) -> AppResult<FingerprintStatus> {
        let url = format!("{}/fingerprint/status", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("fingerprint_status"))?;
        let body: FingerprintStatus = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("fingerprint_status decode"))?;
        Ok(body)
    }

    // ----- Discography sync (Phase 14) -----------------------------------

    /// The cached discography report (`None` when never synced).
    pub async fn discography_report(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Option<DiscographyReport>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            report: Option<DiscographyReport>,
        }
        let url = format!("{}/artists/{artist_id}/discography", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_report"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_report decode"))?;
        Ok(body.report)
    }

    /// Trigger a sync — returns a report, or a candidate list to disambiguate.
    pub async fn discography_sync(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<DiscographySyncResult> {
        let url = format!("{}/artists/{artist_id}/discography/sync", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_sync"))?;
        let body: DiscographySyncResult = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_sync decode"))?;
        Ok(body)
    }

    /// Provider artist candidates for the disambiguation UI.
    pub async fn discography_candidates(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<DiscographyCandidate>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            candidates: Vec<DiscographyCandidate>,
        }
        let url = format!("{}/artists/{artist_id}/discography/candidates", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_candidates"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_candidates decode"))?;
        Ok(body.candidates)
    }

    /// Pin the artist ↔ provider match, or ignore the artist (empty `mbid`).
    pub async fn discography_resolve(
        &self,
        cred: &Credential,
        artist_id: &str,
        mbid: Option<&str>,
    ) -> AppResult<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            mbid: Option<&'a str>,
        }
        let url = format!("{}/artists/{artist_id}/discography/resolve", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&Body { mbid })
            .send()
            .await
            .map_err(rest_err("discography_resolve"))?;
        check_status(resp).await?;
        Ok(())
    }

    /// The artist's suppression list.
    pub async fn discography_ignores(
        &self,
        cred: &Credential,
        artist_id: &str,
    ) -> AppResult<Vec<DiscographyIgnore>> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            ignores: Vec<DiscographyIgnore>,
        }
        let url = format!("{}/artists/{artist_id}/discography/ignores", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_ignores"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_ignores decode"))?;
        Ok(body.ignores)
    }

    /// Suppress a release/track; returns the re-filtered report.
    #[allow(clippy::too_many_arguments)]
    pub async fn discography_add_ignore(
        &self,
        cred: &Credential,
        artist_id: &str,
        scope: &str,
        release_group_id: &str,
        recording_id: Option<&str>,
        title_key: Option<&str>,
        label: &str,
    ) -> AppResult<DiscographyReport> {
        #[derive(Serialize)]
        struct Body<'a> {
            scope: &'a str,
            release_group_id: &'a str,
            recording_id: Option<&'a str>,
            title_key: Option<&'a str>,
            label: &'a str,
        }
        #[derive(Deserialize)]
        struct Resp {
            report: DiscographyReport,
        }
        let url = format!("{}/artists/{artist_id}/discography/ignores", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&Body {
                scope,
                release_group_id,
                recording_id,
                title_key,
                label,
            })
            .send()
            .await
            .map_err(rest_err("discography_add_ignore"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_add_ignore decode"))?;
        Ok(body.report)
    }

    /// Remove a suppression; returns the re-filtered report.
    pub async fn discography_remove_ignore(
        &self,
        cred: &Credential,
        artist_id: &str,
        ignore_id: &str,
    ) -> AppResult<DiscographyReport> {
        #[derive(Deserialize)]
        struct Resp {
            report: DiscographyReport,
        }
        let url = format!(
            "{}/artists/{artist_id}/discography/ignores/{ignore_id}",
            self.base
        );
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_remove_ignore"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_remove_ignore decode"))?;
        Ok(body.report)
    }

    /// Library-wide coverage (`enabled=false` when the server has it off).
    pub async fn discography_status(&self, cred: &Credential) -> AppResult<DiscographyStatus> {
        let url = format!("{}/discography/status", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_status"))?;
        let body: DiscographyStatus = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_status decode"))?;
        Ok(body)
    }

    /// Re-sync every matched artist (rate-limited by the provider).
    pub async fn discography_sync_all(&self, cred: &Credential) -> AppResult<DiscographySyncAll> {
        let url = format!("{}/discography/sync-all", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("discography_sync_all"))?;
        let body: DiscographySyncAll = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("discography_sync_all decode"))?;
        Ok(body)
    }

    // ----- Podcasts ------------------------------------------------------

    pub async fn search_podcasts(
        &self,
        cred: &Credential,
        term: &str,
        limit: i32,
    ) -> AppResult<Vec<PodcastCandidate>> {
        #[derive(Deserialize)]
        struct Resp {
            candidates: Vec<CandidateJson>,
        }
        let url = format!("{}/podcasts/search", self.base);
        let resp = self
            .http
            .get(url)
            .query(&[("term", term), ("limit", &limit.to_string())])
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("search_podcasts"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("search_podcasts decode"))?;
        Ok(body.candidates.into_iter().map(Into::into).collect())
    }

    pub async fn subscribe_feed(
        &self,
        cred: &Credential,
        feed_url: Option<&str>,
        itunes_id: Option<i64>,
    ) -> AppResult<Podcast> {
        let url = format!("{}/podcasts", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "feed_url": feed_url, "itunes_id": itunes_id }))
            .send()
            .await
            .map_err(rest_err("subscribe_feed"))?;
        let body: PodcastJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("subscribe_feed decode"))?;
        Ok(body.into())
    }

    pub async fn list_podcasts(
        &self,
        cred: &Credential,
        limit: i32,
        offset: i32,
    ) -> AppResult<(Vec<Podcast>, i64)> {
        #[derive(Deserialize)]
        struct Resp {
            podcasts: Vec<PodcastJson>,
            total: i64,
        }
        let url = format!("{}/podcasts?limit={limit}&offset={offset}", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_podcasts"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_podcasts decode"))?;
        Ok((body.podcasts.into_iter().map(Into::into).collect(), body.total))
    }

    pub async fn get_podcast(&self, cred: &Credential, id: &str) -> AppResult<Podcast> {
        let url = format!("{}/podcasts/{id}", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("get_podcast"))?;
        let body: PodcastJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("get_podcast decode"))?;
        Ok(body.into())
    }

    pub async fn delete_podcast(&self, cred: &Credential, id: &str) -> AppResult<()> {
        let url = format!("{}/podcasts/{id}", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("delete_podcast"))?;
        check_status(resp).await?;
        Ok(())
    }

    pub async fn refresh_podcast(&self, cred: &Credential, id: &str) -> AppResult<RefreshReport> {
        #[derive(Deserialize)]
        struct Resp {
            podcast_id: String,
            new_episodes: i64,
            not_modified: bool,
        }
        let url = format!("{}/podcasts/{id}/refresh", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("refresh_podcast"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("refresh_podcast decode"))?;
        Ok(RefreshReport {
            podcast_id: body.podcast_id,
            new_episodes: body.new_episodes,
            not_modified: body.not_modified,
        })
    }

    pub async fn set_podcast_auto_download(
        &self,
        cred: &Credential,
        id: &str,
        auto_download: i32,
    ) -> AppResult<Podcast> {
        let url = format!("{}/podcasts/{id}/auto-download", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "auto_download": auto_download }))
            .send()
            .await
            .map_err(rest_err("set_auto_download"))?;
        let body: PodcastJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("set_auto_download decode"))?;
        Ok(body.into())
    }

    pub async fn list_episodes(
        &self,
        cred: &Credential,
        podcast_id: &str,
        limit: i32,
        offset: i32,
    ) -> AppResult<Vec<PodcastEpisode>> {
        #[derive(Deserialize)]
        struct Resp {
            episodes: Vec<EpisodeJson>,
        }
        let url = format!(
            "{}/podcasts/{podcast_id}/episodes?limit={limit}&offset={offset}",
            self.base
        );
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_episodes"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_episodes decode"))?;
        Ok(body.episodes.into_iter().map(Into::into).collect())
    }

    pub async fn get_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        let url = format!("{}/podcasts/episodes/{id}", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("get_episode"))?;
        let body: EpisodeJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("get_episode decode"))?;
        Ok(body.into())
    }

    pub async fn download_episode(&self, cred: &Credential, id: &str) -> AppResult<PodcastEpisode> {
        let url = format!("{}/podcasts/episodes/{id}/download", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("download_episode"))?;
        let body: EpisodeJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("download_episode decode"))?;
        Ok(body.into())
    }

    pub async fn subscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        let url = format!("{}/podcasts/{id}/subscribe", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("subscribe"))?;
        check_status(resp).await?;
        Ok(true)
    }

    pub async fn unsubscribe_podcast(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        let url = format!("{}/podcasts/{id}/subscribe", self.base);
        let resp = self
            .http
            .delete(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("unsubscribe"))?;
        check_status(resp).await?;
        Ok(false)
    }

    pub async fn is_subscribed(&self, cred: &Credential, id: &str) -> AppResult<bool> {
        #[derive(Deserialize)]
        struct Resp {
            subscribed: bool,
        }
        let url = format!("{}/podcasts/{id}/subscribe", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("is_subscribed"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("is_subscribed decode"))?;
        Ok(body.subscribed)
    }

    pub async fn list_subscriptions(&self, cred: &Credential) -> AppResult<Vec<Podcast>> {
        #[derive(Deserialize)]
        struct Resp {
            podcasts: Vec<PodcastJson>,
        }
        let url = format!("{}/podcasts/subscriptions", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_subscriptions"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_subscriptions decode"))?;
        Ok(body.podcasts.into_iter().map(Into::into).collect())
    }

    pub async fn record_episode_progress(
        &self,
        cred: &Credential,
        episode_id: &str,
        position_ms: i64,
        completed: bool,
    ) -> AppResult<EpisodeProgress> {
        let url = format!("{}/podcasts/episodes/{episode_id}/progress", self.base);
        let resp = self
            .http
            .put(url)
            .header("authorization", auth_header(cred))
            .json(&serde_json::json!({ "position_ms": position_ms, "completed": completed }))
            .send()
            .await
            .map_err(rest_err("record_episode_progress"))?;
        let body: ProgressJson = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("record_episode_progress decode"))?;
        Ok(body.into())
    }

    pub async fn list_episode_progress(
        &self,
        cred: &Credential,
        podcast_id: &str,
    ) -> AppResult<Vec<EpisodeProgress>> {
        #[derive(Deserialize)]
        struct Resp {
            progress: Vec<ProgressJson>,
        }
        let url = format!("{}/podcasts/{podcast_id}/progress", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_episode_progress"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_episode_progress decode"))?;
        Ok(body.progress.into_iter().map(Into::into).collect())
    }

    // ----- Image upload (Phase 9; Manager+ gated, REST-only binary blob) ----

    /// `POST /albums/:id/cover` — raw `image/*` body. Returns `()`; the caller
    /// refreshes the album view (the cover is served via `GET .../cover`).
    pub async fn upload_album_cover(
        &self,
        cred: &Credential,
        album_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        let url = format!("{}/albums/{album_id}/cover", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await
            .map_err(rest_err("upload_album_cover"))?;
        check_status(resp).await?;
        Ok(())
    }

    /// `POST /artists/:id/image` — raw `image/*` body.
    pub async fn upload_artist_image(
        &self,
        cred: &Credential,
        artist_id: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> AppResult<()> {
        let url = format!("{}/artists/{artist_id}/image", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await
            .map_err(rest_err("upload_artist_image"))?;
        check_status(resp).await?;
        Ok(())
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

    // ----- Rescan library (Phase 8+) ---------------------------------------

    /// `POST /library/rescan` — re-measure actual duration for all tracks.
    pub async fn rescan_library(&self, cred: &Credential) -> AppResult<RescanReport> {
        let url = format!("{}/library/rescan", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("rescan_library"))?;
        let body: RescanDto = check_status(resp).await?.json().await.map_err(rest_err("rescan_library decode"))?;
        Ok(RescanReport {
            tracks_checked: body.tracks_checked,
            tracks_updated: body.tracks_updated,
            errors: body.errors,
        })
    }

    // ----- Uploads (Phase 8) -----------------------------------------------

    /// Upload a file (single audio or archive) via REST multipart `POST /upload`.
    /// Manager+ required. Response is either `{track_id, path}` (single) or
    /// `{kind, ingested, ...}` (archive).
    pub async fn upload_file(
        &self,
        cred: &Credential,
        filename: String,
        data: Vec<u8>,
        cover: Option<(String, Vec<u8>)>,
    ) -> AppResult<UploadResult> {
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename)
            .mime_str("application/octet-stream")
            .map_err(|e| AppError::Transport(format!("mime: {e}")))?;
        let mut form = reqwest::multipart::Form::new().part("file", part);

        // Optional album cover as a second `cover` field; the server stages
        // it as a sidecar so ingest picks it up before any remote fetch.
        if let Some((cover_name, cover_bytes)) = cover {
            let cover_part = reqwest::multipart::Part::bytes(cover_bytes)
                .file_name(cover_name)
                .mime_str("image/jpeg")
                .map_err(|e| AppError::Transport(format!("cover mime: {e}")))?;
            form = form.part("cover", cover_part);
        }

        let url = format!("{}/upload", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .multipart(form)
            .send()
            .await
            .map_err(rest_err("upload"))?;
        let body = check_status(resp).await?;

        // The server returns an untagged enum: try archive shape first, then single.
        let text = body
            .text()
            .await
            .map_err(rest_err("upload body"))?;
        parse_upload_response(&text)
    }

    // ----- Uploads v2 (sessions + reports + live stream) -------------------

    pub async fn init_upload(
        &self,
        cred: &Credential,
        body: &UploadInitRequest,
    ) -> AppResult<UploadView> {
        let url = format!("{}/uploads/init", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .json(body)
            .send()
            .await
            .map_err(rest_err("init_upload"))?;
        check_status(resp)
            .await?
            .json::<UploadView>()
            .await
            .map_err(rest_err("init_upload decode"))
    }

    pub async fn put_chunk(
        &self,
        cred: &Credential,
        upload_id: &str,
        file_index: u32,
        chunk_index: u32,
        data: Vec<u8>,
    ) -> AppResult<ChunkAck> {
        let url = format!(
            "{}/uploads/{upload_id}/files/{file_index}/chunks/{chunk_index}",
            self.base
        );
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .header("content-type", "application/octet-stream")
            .body(data)
            .send()
            .await
            .map_err(rest_err("put_chunk"))?;
        check_status(resp)
            .await?
            .json::<ChunkAck>()
            .await
            .map_err(rest_err("put_chunk decode"))
    }

    pub async fn get_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let url = format!("{}/uploads/{id}", self.base);
        let resp = self
            .http
            .get(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("get_upload"))?;
        check_status(resp)
            .await?
            .json::<UploadView>()
            .await
            .map_err(rest_err("get_upload decode"))
    }

    pub async fn list_uploads(
        &self,
        cred: &Credential,
        filter: &UploadListFilter,
    ) -> AppResult<Vec<UploadSummary>> {
        #[derive(Deserialize)]
        struct Resp {
            uploads: Vec<UploadSummary>,
        }
        let mut query: Vec<(String, String)> = Vec::new();
        if let Some(u) = &filter.user_id {
            query.push(("user_id".into(), u.clone()));
        }
        if let Some(s) = &filter.state {
            query.push(("state".into(), s.clone()));
        }
        if let Some(l) = filter.limit {
            query.push(("limit".into(), l.to_string()));
        }
        if let Some(o) = filter.offset {
            query.push(("offset".into(), o.to_string()));
        }
        let url = format!("{}/uploads", self.base);
        let resp = self
            .http
            .get(url)
            .query(&query)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("list_uploads"))?;
        let body: Resp = check_status(resp)
            .await?
            .json()
            .await
            .map_err(rest_err("list_uploads decode"))?;
        Ok(body.uploads)
    }

    pub async fn cancel_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let url = format!("{}/uploads/{id}/cancel", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("cancel_upload"))?;
        check_status(resp)
            .await?
            .json::<UploadView>()
            .await
            .map_err(rest_err("cancel_upload decode"))
    }

    pub async fn pause_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let url = format!("{}/uploads/{id}/pause", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("pause_upload"))?;
        check_status(resp)
            .await?
            .json::<UploadView>()
            .await
            .map_err(rest_err("pause_upload decode"))
    }

    pub async fn resume_upload(&self, cred: &Credential, id: &str) -> AppResult<UploadView> {
        let url = format!("{}/uploads/{id}/resume", self.base);
        let resp = self
            .http
            .post(url)
            .header("authorization", auth_header(cred))
            .send()
            .await
            .map_err(rest_err("resume_upload"))?;
        check_status(resp)
            .await?
            .json::<UploadView>()
            .await
            .map_err(rest_err("resume_upload decode"))
    }

    /// Open the live `uploads` WebSocket (REST-side fallback for the gRPC
    /// stream). Spawns a reader that forwards permitted events to `tx` until
    /// the socket closes; the auth credential rides the handshake header.
    pub async fn stream_uploads(
        &self,
        cred: &Credential,
        tx: tokio::sync::mpsc::Sender<UploadEvent>,
    ) -> AppResult<()> {
        use futures_util::StreamExt;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
        use tokio_tungstenite::tungstenite::http::HeaderValue;
        use tokio_tungstenite::tungstenite::Message;

        // http(s) base → ws(s) URL.
        let ws_base = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            format!("ws://{}", self.base)
        };
        let url = format!("{ws_base}/uploads/stream");

        let mut request = url
            .into_client_request()
            .map_err(|e| AppError::Transport(format!("ws request: {e}")))?;
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_header(cred))
                .map_err(|e| AppError::Transport(format!("ws auth header: {e}")))?,
        );

        let (ws_stream, _resp) = connect_async(request)
            .await
            .map_err(|e| AppError::Transport(format!("ws connect: {e}")))?;
        let (_write, mut read) = ws_stream.split();
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(txt)) => {
                        if let Ok(ev) = serde_json::from_str::<UploadEvent>(&txt) {
                            if tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        });
        Ok(())
    }
}

// REST DTOs (match server/src/rest/library.rs exactly).

#[derive(Deserialize)]
struct AliasJson {
    id: String,
    name: String,
    #[serde(default)]
    sort_name: Option<String>,
    #[serde(default)]
    language: Option<String>,
    is_primary: bool,
}
impl From<AliasJson> for super::AliasInfo {
    fn from(a: AliasJson) -> Self {
        Self {
            id: a.id,
            name: a.name,
            sort_name: a.sort_name,
            language: a.language,
            is_primary: a.is_primary,
        }
    }
}

#[derive(Deserialize)]
struct ArtistJson {
    id: String,
    name: String,
    sort_name: Option<String>,
    #[serde(default)]
    image_path: Option<String>,
    #[serde(default)]
    aliases: Vec<AliasJson>,
    #[serde(default)]
    storage_bytes: i64,
}
impl From<ArtistJson> for Artist {
    fn from(a: ArtistJson) -> Self {
        Self {
            id: a.id,
            name: a.name,
            sort_name: a.sort_name,
            image_path: a.image_path,
            aliases: a.aliases.into_iter().map(Into::into).collect(),
            storage_bytes: a.storage_bytes,
        }
    }
}

#[derive(Deserialize)]
struct AlbumJson {
    id: String,
    artist_id: String,
    title: String,
    release_year: Option<i64>,
    #[serde(default = "super::default_album_type")]
    album_type: String,
    #[serde(default)]
    is_explicit: bool,
    cover_path: Option<String>,
    #[serde(default)]
    aliases: Vec<AliasJson>,
    #[serde(default)]
    storage_bytes: i64,
}
impl From<AlbumJson> for Album {
    fn from(a: AlbumJson) -> Self {
        Self {
            id: a.id,
            artist_id: a.artist_id,
            title: a.title,
            release_year: a.release_year,
            album_type: a.album_type,
            is_explicit: a.is_explicit,
            cover_path: a.cover_path,
            aliases: a.aliases.into_iter().map(Into::into).collect(),
            storage_bytes: a.storage_bytes,
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
    #[serde(default)]
    sample_rate_hz: Option<i64>,
    #[serde(default)]
    bit_depth: Option<i64>,
    #[serde(default)]
    channels: Option<i64>,
    metadata_json: String,
    #[serde(default)]
    is_single_release: bool,
    #[serde(default)]
    is_explicit: bool,
    #[serde(default)]
    aliases: Vec<AliasJson>,
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
            sample_rate_hz: t.sample_rate_hz,
            bit_depth: t.bit_depth,
            channels: t.channels,
            metadata_json: t.metadata_json,
            is_single_release: t.is_single_release,
            is_explicit: t.is_explicit,
            aliases: t.aliases.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Deserialize)]
struct LibraryStorageJson {
    #[serde(default)]
    music_bytes: i64,
    #[serde(default)]
    podcast_bytes: i64,
    #[serde(default)]
    artwork_bytes: i64,
    #[serde(default)]
    other_bytes: i64,
    #[serde(default)]
    total_bytes: i64,
    #[serde(default)]
    track_count: i64,
    #[serde(default)]
    album_count: i64,
    #[serde(default)]
    artist_count: i64,
    #[serde(default)]
    podcast_count: i64,
    #[serde(default)]
    episode_count: i64,
    #[serde(default)]
    computed_at: String,
}
impl From<LibraryStorageJson> for LibraryStorage {
    fn from(s: LibraryStorageJson) -> Self {
        Self {
            music_bytes: s.music_bytes,
            podcast_bytes: s.podcast_bytes,
            artwork_bytes: s.artwork_bytes,
            other_bytes: s.other_bytes,
            total_bytes: s.total_bytes,
            track_count: s.track_count,
            album_count: s.album_count,
            artist_count: s.artist_count,
            podcast_count: s.podcast_count,
            episode_count: s.episode_count,
            computed_at: s.computed_at,
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

#[derive(Deserialize)]
struct PodcastJson {
    id: String,
    feed_url: String,
    title: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    itunes_id: Option<i64>,
    #[serde(default)]
    podcastindex_id: Option<i64>,
    auto_download: i32,
    #[serde(default)]
    last_refreshed_at: Option<String>,
    #[serde(default)]
    storage_bytes: i64,
}
impl From<PodcastJson> for Podcast {
    fn from(p: PodcastJson) -> Self {
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
            storage_bytes: p.storage_bytes,
        }
    }
}

#[derive(Deserialize)]
struct CandidateJson {
    feed_url: String,
    title: String,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    itunes_id: Option<i64>,
    #[serde(default)]
    podcastindex_id: Option<i64>,
}
impl From<CandidateJson> for PodcastCandidate {
    fn from(c: CandidateJson) -> Self {
        Self {
            feed_url: c.feed_url,
            title: c.title,
            author: c.author,
            description: c.description,
            image_url: c.image_url,
            categories: c.categories,
            itunes_id: c.itunes_id,
            podcastindex_id: c.podcastindex_id,
        }
    }
}

#[derive(Deserialize)]
struct EpisodeJson {
    id: String,
    podcast_id: String,
    guid: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    enclosure_url: String,
    #[serde(default)]
    enclosure_type: Option<String>,
    #[serde(default)]
    episode_no: Option<i64>,
    #[serde(default)]
    season_no: Option<i64>,
    #[serde(default)]
    duration_ms: Option<i64>,
    #[serde(default)]
    codec: Option<String>,
    #[serde(default)]
    bitrate_kbps: Option<i64>,
    #[serde(default)]
    file_size: Option<i64>,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    downloaded: bool,
}
impl From<EpisodeJson> for PodcastEpisode {
    fn from(e: EpisodeJson) -> Self {
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
            downloaded: e.downloaded,
        }
    }
}

#[derive(Deserialize)]
struct ProgressJson {
    episode_id: String,
    position_ms: i64,
    completed: bool,
    updated_at: String,
}
impl From<ProgressJson> for EpisodeProgress {
    fn from(p: ProgressJson) -> Self {
        Self {
            episode_id: p.episode_id,
            position_ms: p.position_ms,
            completed: p.completed,
            updated_at: p.updated_at,
        }
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

#[derive(Deserialize)]
struct NotificationJson {
    id: String,
    kind: String,
    #[serde(default)]
    artist_id: Option<String>,
    #[serde(default)]
    album_id: Option<String>,
    #[serde(default)]
    podcast_id: Option<String>,
    #[serde(default)]
    episode_id: Option<String>,
    title: String,
    #[serde(default)]
    body: Option<String>,
    read: bool,
    created_at: String,
}
impl From<NotificationJson> for super::Notification {
    fn from(n: NotificationJson) -> Self {
        Self {
            id: n.id,
            kind: n.kind,
            artist_id: n.artist_id,
            album_id: n.album_id,
            podcast_id: n.podcast_id,
            episode_id: n.episode_id,
            title: n.title,
            body: n.body,
            read: n.read,
            created_at: n.created_at,
        }
    }
}

#[derive(Deserialize)]
struct NotificationPageJson {
    notifications: Vec<NotificationJson>,
    total: i64,
    unread_count: i64,
}
impl From<NotificationPageJson> for super::NotificationPage {
    fn from(p: NotificationPageJson) -> Self {
        Self {
            notifications: p.notifications.into_iter().map(Into::into).collect(),
            total: p.total,
            unread_count: p.unread_count,
        }
    }
}

#[derive(Deserialize)]
struct PlayEventJson {
    id: String,
    #[serde(default)]
    track_id: Option<String>,
    #[serde(default)]
    artist_id: Option<String>,
    #[serde(default)]
    album_id: Option<String>,
    track_title: String,
    artist_name: String,
    ms_played: i64,
    completed: bool,
    played_at: String,
}
impl From<PlayEventJson> for PlayEvent {
    fn from(p: PlayEventJson) -> Self {
        Self {
            id: p.id,
            track_id: p.track_id,
            artist_id: p.artist_id,
            album_id: p.album_id,
            track_title: p.track_title,
            artist_name: p.artist_name,
            ms_played: p.ms_played,
            completed: p.completed,
            played_at: p.played_at,
        }
    }
}

#[derive(Deserialize)]
struct PlayHistoryPageJson {
    events: Vec<PlayEventJson>,
    total: i64,
}
impl From<PlayHistoryPageJson> for PlayHistoryPage {
    fn from(p: PlayHistoryPageJson) -> Self {
        Self {
            events: p.events.into_iter().map(Into::into).collect(),
            total: p.total,
        }
    }
}

#[derive(Deserialize)]
struct TrackStatJson {
    #[serde(default)]
    track_id: Option<String>,
    track_title: String,
    artist_name: String,
    plays: i64,
}
impl From<TrackStatJson> for TrackStat {
    fn from(s: TrackStatJson) -> Self {
        Self {
            track_id: s.track_id,
            track_title: s.track_title,
            artist_name: s.artist_name,
            plays: s.plays,
        }
    }
}

#[derive(Deserialize)]
struct ArtistStatJson {
    #[serde(default)]
    artist_id: Option<String>,
    artist_name: String,
    plays: i64,
}
impl From<ArtistStatJson> for ArtistStat {
    fn from(s: ArtistStatJson) -> Self {
        Self {
            artist_id: s.artist_id,
            artist_name: s.artist_name,
            plays: s.plays,
        }
    }
}

#[derive(Deserialize)]
struct StatsJson {
    top_tracks: Vec<TrackStatJson>,
    top_artists: Vec<ArtistStatJson>,
    total_plays: i64,
    total_ms: i64,
}
impl From<StatsJson> for ListeningStats {
    fn from(s: StatsJson) -> Self {
        Self {
            top_tracks: s.top_tracks.into_iter().map(Into::into).collect(),
            top_artists: s.top_artists.into_iter().map(Into::into).collect(),
            total_plays: s.total_plays,
            total_ms: s.total_ms,
        }
    }
}

/// Map a favorite `kind` to its REST path segment (`track` → `tracks`, etc.).
fn fav_path_segment(kind: &str) -> AppResult<&'static str> {
    match kind {
        "track" => Ok("tracks"),
        "album" => Ok("albums"),
        "artist" => Ok("artists"),
        other => Err(AppError::Transport(format!("invalid favorite kind: {other}"))),
    }
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

/// Inverse of `parse_tier` — the lowercase string the server's
/// `PermissionLevel` (serde `rename_all = "lowercase"`) expects on the
/// `POST /auth/register` body.
fn tier_to_rest_str(tier: super::PermissionTier) -> &'static str {
    match tier {
        super::PermissionTier::Admin => "admin",
        super::PermissionTier::Manager => "manager",
        super::PermissionTier::User => "user",
    }
}

/// Parse the server's `POST /upload` response. The body is an untagged enum:
/// either `{track_id, path}` (single) or `{kind, ingested, ...}` (archive).
fn parse_upload_response(text: &str) -> AppResult<UploadResult> {
    // Try archive shape first (it has `kind` which is unique).
    if let Ok(a) = serde_json::from_str::<serde_json::Value>(text) {
        if a.get("kind").is_some() {
            let archive: ArchiveUploadResponse = serde_json::from_str(text)
                .map_err(|e| AppError::Transport(format!("upload archive decode: {e}")))?;
            return Ok(UploadResult::Archive(ArchiveUploadResult {
                kind: archive.kind,
                ingested: archive.ingested,
                already_indexed: archive.already_indexed,
                non_audio_skipped: archive.non_audio_skipped,
                errors: archive.errors,
                track_ids: archive.track_ids,
            }));
        }
    }
    // Fall back to single.
    let single: SingleUploadResponse =
        serde_json::from_str(text)
            .map_err(|e| AppError::Transport(format!("upload single decode: {e}")))?;
    Ok(UploadResult::Single(SingleUploadResult {
        track_id: single.track_id,
        path: single.path,
    }))
}

#[derive(Deserialize)]
struct SingleUploadResponse {
    track_id: String,
    path: String,
}

#[derive(Deserialize)]
struct ArchiveUploadResponse {
    kind: String,
    ingested: u64,
    already_indexed: u64,
    non_audio_skipped: u64,
    errors: u64,
    track_ids: Vec<String>,
}

#[derive(Deserialize)]
struct RescanDto {
    tracks_checked: u64,
    tracks_updated: u64,
    errors: u64,
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
