//! gRPC LibraryService implementation.

use std::path::PathBuf;

use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::auth::Identity;
use crate::db::models::{self as m, NewTrack};
use crate::error::AppError;
use crate::grpc::auth_svc::map_err;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::proto::library as pb;
use crate::services::{
    ArtworkService, LibraryService, MetadataEdit, MetadataService, ScanService, StorageService,
};
use crate::time_fmt::rfc3339;

#[derive(Clone)]
pub struct LibraryServer {
    pub library: LibraryService,
    pub scan: ScanService,
    pub storage: StorageService,
    pub metadata: MetadataService,
    pub artwork: Option<ArtworkService>,
    pub interceptor: AuthInterceptor,
}

impl LibraryServer {
    pub fn into_service(self) -> pb::library_service_server::LibraryServiceServer<Self> {
        pb::library_service_server::LibraryServiceServer::new(self)
    }

    async fn caller<T>(&self, req: &Request<T>) -> Result<Identity, Status> {
        self.interceptor.resolve(req).await
    }

    /// Build a `pb::Artist` with its alias list populated (single-entity reads).
    async fn artist_pb_with_aliases(
        &self,
        caller: &Identity,
        a: m::Artist,
    ) -> Result<pb::Artist, Status> {
        let aliases = self
            .library
            .list_artist_aliases(caller, a.id)
            .await
            .map_err(map_err)?;
        let mut pb = artist_to_pb(a);
        pb.aliases = aliases.into_iter().map(artist_alias_to_pb).collect();
        Ok(pb)
    }

    /// Build a `pb::Album` with its alias list populated (single-entity reads).
    async fn album_pb_with_aliases(
        &self,
        caller: &Identity,
        a: m::Album,
    ) -> Result<pb::Album, Status> {
        let aliases = self
            .library
            .list_album_aliases(caller, a.id)
            .await
            .map_err(map_err)?;
        let mut pb = album_to_pb(a);
        pb.aliases = aliases.into_iter().map(album_alias_to_pb).collect();
        Ok(pb)
    }

    /// Build a `pb::Track` with its alias list populated (single-entity reads).
    async fn track_pb_with_aliases(
        &self,
        caller: &Identity,
        t: m::Track,
    ) -> Result<pb::Track, Status> {
        let aliases = self
            .library
            .list_track_aliases(caller, t.id)
            .await
            .map_err(map_err)?;
        let mut pb = track_to_pb(t);
        pb.aliases = aliases.into_iter().map(track_alias_to_pb).collect();
        Ok(pb)
    }
}

fn parse_uuid(s: &str, what: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(s).map_err(|_| Status::invalid_argument(format!("invalid {what} uuid")))
}

fn nonempty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn nz_i32(v: i32) -> Option<i32> { if v == 0 { None } else { Some(v) } }
fn nz_i64(v: i64) -> Option<i64> { if v == 0 { None } else { Some(v) } }

fn page_of(p: Option<pb::Pagination>) -> (Option<i64>, Option<i64>) {
    match p {
        Some(p) => (Some(p.limit), Some(p.offset)),
        None => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Model <-> proto conversions
// ---------------------------------------------------------------------------

fn artist_to_pb(a: m::Artist) -> pb::Artist {
    pb::Artist {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name.unwrap_or_default(),
        created_at: rfc3339(a.created_at),
        updated_at: rfc3339(a.updated_at),
        image_path: a.image_path.unwrap_or_default(),
        aliases: Vec::new(),
        storage_bytes: a.storage_bytes,
    }
}
fn album_to_pb(a: m::Album) -> pb::Album {
    pb::Album {
        id: a.id.to_string(),
        artist_id: a.artist_id.to_string(),
        title: a.title,
        release_year: a.release_year.unwrap_or(0),
        cover_path: a.cover_path.unwrap_or_default(),
        created_at: rfc3339(a.created_at),
        updated_at: rfc3339(a.updated_at),
        aliases: Vec::new(),
        storage_bytes: a.storage_bytes,
        album_type: a.album_type,
    }
}
fn track_to_pb(t: m::Track) -> pb::Track {
    pb::Track {
        id: t.id.to_string(),
        album_id: t.album_id.to_string(),
        artist_id: t.artist_id.to_string(),
        title: t.title,
        track_no: t.track_no.unwrap_or(0),
        disc_no: t.disc_no.unwrap_or(0),
        duration_ms: t.duration_ms,
        codec: t.codec,
        bitrate_kbps: t.bitrate_kbps.unwrap_or(0),
        file_path: t.file_path,
        file_size: t.file_size.unwrap_or(0),
        metadata_json: t.metadata_json,
        created_at: rfc3339(t.created_at),
        updated_at: rfc3339(t.updated_at),
        is_single_release: t.is_single_release,
        sample_rate_hz: t.sample_rate_hz.unwrap_or(0),
        bit_depth: t.bit_depth.unwrap_or(0),
        channels: t.channels.unwrap_or(0),
        aliases: Vec::new(),
    }
}
fn library_storage_to_pb(s: m::LibraryStorage) -> pb::LibraryStorage {
    pb::LibraryStorage {
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
        computed_at: rfc3339(s.computed_at),
    }
}
fn artist_alias_to_pb(a: m::ArtistAlias) -> pb::AliasInfo {
    pb::AliasInfo {
        id: a.id.to_string(),
        name: a.name,
        sort_name: a.sort_name.unwrap_or_default(),
        language: a.language.unwrap_or_default(),
        is_primary: a.is_primary,
    }
}
fn album_alias_to_pb(a: m::AlbumAlias) -> pb::AliasInfo {
    pb::AliasInfo {
        id: a.id.to_string(),
        name: a.title,
        sort_name: String::new(),
        language: a.language.unwrap_or_default(),
        is_primary: a.is_primary,
    }
}
fn track_alias_to_pb(a: m::TrackAlias) -> pb::AliasInfo {
    pb::AliasInfo {
        id: a.id.to_string(),
        name: a.title,
        sort_name: String::new(),
        language: a.language.unwrap_or_default(),
        is_primary: a.is_primary,
    }
}

// ---------------------------------------------------------------------------
// RPC impl
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl pb::library_service_server::LibraryService for LibraryServer {
    // ---- Artists ----
    async fn create_artist(
        &self,
        req: Request<pb::CreateArtistRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let sort = nonempty(body.sort_name);
        let artist = self
            .library
            .create_artist(&caller, &body.name, sort.as_deref())
            .await
            .map_err(map_err)?;
        Ok(Response::new(artist_to_pb(artist)))
    }
    async fn get_artist(
        &self,
        req: Request<pb::GetArtistRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "artist")?;
        let a = self.library.get_artist(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn update_artist(
        &self,
        req: Request<pb::UpdateArtistRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.id, "artist")?;
        let sort = nonempty(body.sort_name);
        let a = self
            .library
            .update_artist(&caller, id, &body.name, sort.as_deref())
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn delete_artist(
        &self,
        req: Request<pb::DeleteArtistRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "artist")?;
        let deleted = self.library.delete_artist(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse { deleted }))
    }
    async fn list_artists(
        &self,
        req: Request<pb::ListArtistsRequest>,
    ) -> Result<Response<pb::ListArtistsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let (limit, offset) = page_of(req.into_inner().page);
        let (rows, total) = self
            .library
            .list_artists(&caller, limit, offset)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListArtistsResponse {
            artists: rows.into_iter().map(artist_to_pb).collect(),
            total,
        }))
    }
    async fn search_artists(
        &self,
        req: Request<pb::SearchRequest>,
    ) -> Result<Response<pb::ListArtistsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let (limit, offset) = page_of(body.page);
        let rows = self
            .library
            .search_artists(&caller, &body.query, limit, offset)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListArtistsResponse {
            artists: rows.into_iter().map(artist_to_pb).collect(),
            total,
        }))
    }

    // ---- Albums ----
    async fn create_album(
        &self,
        req: Request<pb::CreateAlbumRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let artist_id = parse_uuid(&body.artist_id, "artist")?;
        let cover = nonempty(body.cover_path);
        let album = self
            .library
            .create_album(
                &caller,
                artist_id,
                &body.title,
                nz_i32(body.release_year),
                cover.as_deref(),
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(album_to_pb(album)))
    }
    async fn get_album(
        &self,
        req: Request<pb::GetAlbumRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "album")?;
        let a = self.library.get_album(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }
    async fn update_album(
        &self,
        req: Request<pb::UpdateAlbumRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let id = parse_uuid(&body.id, "album")?;
        let cover = nonempty(body.cover_path);
        let a = self
            .library
            .update_album(
                &caller,
                id,
                &body.title,
                nz_i32(body.release_year),
                cover.as_deref(),
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }
    async fn delete_album(
        &self,
        req: Request<pb::DeleteAlbumRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "album")?;
        let deleted = self.library.delete_album(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse { deleted }))
    }
    async fn list_albums_by_artist(
        &self,
        req: Request<pb::ListAlbumsByArtistRequest>,
    ) -> Result<Response<pb::ListAlbumsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let artist_id = parse_uuid(&req.into_inner().artist_id, "artist")?;
        let rows = self
            .library
            .list_albums_by_artist(&caller, artist_id)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListAlbumsResponse {
            albums: rows.into_iter().map(album_to_pb).collect(),
            total,
        }))
    }
    async fn search_albums(
        &self,
        req: Request<pb::SearchRequest>,
    ) -> Result<Response<pb::ListAlbumsResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let (limit, offset) = page_of(body.page);
        let rows = self
            .library
            .search_albums(&caller, &body.query, limit, offset)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListAlbumsResponse {
            albums: rows.into_iter().map(album_to_pb).collect(),
            total,
        }))
    }

    // ---- Tracks ----
    async fn create_track(
        &self,
        req: Request<pb::CreateTrackRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let album_id = parse_uuid(&b.album_id, "album")?;
        let artist_id = parse_uuid(&b.artist_id, "artist")?;
        let metadata_json = if b.metadata_json.is_empty() {
            "{}".to_string()
        } else {
            b.metadata_json
        };
        let new = NewTrack {
            album_id,
            artist_id,
            title: b.title,
            track_no: nz_i32(b.track_no),
            disc_no: nz_i32(b.disc_no),
            duration_ms: b.duration_ms,
            codec: b.codec,
            bitrate_kbps: nz_i32(b.bitrate_kbps),
            file_path: b.file_path,
            file_size: nz_i64(b.file_size),
            // Manual catalog create carries no probe — quality detail is filled
            // by a subsequent rescan/probe of the on-disk file.
            sample_rate_hz: None,
            bit_depth: None,
            channels: None,
            metadata_json,
        };
        let t = self.library.create_track(&caller, new).await.map_err(map_err)?;
        Ok(Response::new(track_to_pb(t)))
    }
    async fn get_track(
        &self,
        req: Request<pb::GetTrackRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "track")?;
        let t = self.library.get_track(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(self.track_pb_with_aliases(&caller, t).await?))
    }
    async fn update_track(
        &self,
        req: Request<pb::UpdateTrackRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.id, "track")?;
        let meta = if b.metadata_json.is_empty() {
            "{}".to_string()
        } else {
            b.metadata_json
        };
        let t = self
            .library
            .update_track(&caller, id, &b.title, nz_i32(b.track_no), nz_i32(b.disc_no), &meta)
            .await
            .map_err(map_err)?;
        Ok(Response::new(track_to_pb(t)))
    }
    async fn delete_track(
        &self,
        req: Request<pb::DeleteTrackRequest>,
    ) -> Result<Response<pb::DeleteResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "track")?;
        let deleted = self.library.delete_track(&caller, id).await.map_err(map_err)?;
        Ok(Response::new(pb::DeleteResponse { deleted }))
    }
    async fn list_tracks_by_album(
        &self,
        req: Request<pb::ListTracksByAlbumRequest>,
    ) -> Result<Response<pb::ListTracksResponse>, Status> {
        let caller = self.caller(&req).await?;
        let album_id = parse_uuid(&req.into_inner().album_id, "album")?;
        let rows = self
            .library
            .list_tracks_by_album(&caller, album_id)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListTracksResponse {
            tracks: rows.into_iter().map(track_to_pb).collect(),
            total,
        }))
    }
    async fn search_tracks(
        &self,
        req: Request<pb::SearchRequest>,
    ) -> Result<Response<pb::ListTracksResponse>, Status> {
        let caller = self.caller(&req).await?;
        let body = req.into_inner();
        let (limit, offset) = page_of(body.page);
        let rows = self
            .library
            .search_tracks(&caller, &body.query, limit, offset)
            .await
            .map_err(map_err)?;
        let total = rows.len() as i64;
        Ok(Response::new(pb::ListTracksResponse {
            tracks: rows.into_iter().map(track_to_pb).collect(),
            total,
        }))
    }

    // ---- Metadata & Artwork ----
    async fn edit_track_metadata(
        &self,
        req: Request<pb::EditTrackMetadataRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.id, "track")?;
        let edit = MetadataEdit {
            title: b.title,
            track_no: b.track_no,
            disc_no: b.disc_no,
            metadata_json: b.metadata_json,
            year: b.year,
        };
        let t = self
            .metadata
            .edit_track(&caller, id, edit)
            .await
            .map_err(map_err)?;
        Ok(Response::new(track_to_pb(t)))
    }
    async fn fetch_album_artwork(
        &self,
        req: Request<pb::FetchAlbumArtworkRequest>,
    ) -> Result<Response<pb::FetchAlbumArtworkResponse>, Status> {
        let caller = self.caller(&req).await?;
        let album_id = parse_uuid(&req.into_inner().album_id, "album")?;
        let artwork = self
            .artwork
            .as_ref()
            .filter(|a| a.auto_fetch_enabled())
            .ok_or_else(|| {
                Status::failed_precondition("artwork fetch is disabled (set FETCH_ARTWORK)")
            })?;
        let cover = artwork
            .fetch_for_album(&caller, album_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::FetchAlbumArtworkResponse {
            found: cover.is_some(),
            cover_path: cover.unwrap_or_default(),
        }))
    }

    // ---- Merge + aliases ----
    async fn merge_artists(
        &self,
        req: Request<pb::MergeRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let survivor = parse_uuid(&b.survivor_id, "artist")?;
        let duplicate = parse_uuid(&b.duplicate_id, "artist")?;
        let a = self
            .library
            .merge_artists(&caller, survivor, duplicate)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn merge_albums(
        &self,
        req: Request<pb::MergeRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let survivor = parse_uuid(&b.survivor_id, "album")?;
        let duplicate = parse_uuid(&b.duplicate_id, "album")?;
        let a = self
            .library
            .merge_albums(&caller, survivor, duplicate)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }
    async fn move_track(
        &self,
        req: Request<pb::MoveTrackRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let track_id = parse_uuid(&b.track_id, "track")?;
        let album_id = parse_uuid(&b.album_id, "album")?;
        let t = self
            .library
            .move_track(&caller, track_id, album_id, b.single_release)
            .await
            .map_err(map_err)?;
        Ok(Response::new(track_to_pb(t)))
    }
    async fn set_track_single_release(
        &self,
        req: Request<pb::SetSingleReleaseRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let track_id = parse_uuid(&b.track_id, "track")?;
        let t = self
            .library
            .set_track_single_release(&caller, track_id, b.single_release)
            .await
            .map_err(map_err)?;
        Ok(Response::new(track_to_pb(t)))
    }
    async fn set_album_type(
        &self,
        req: Request<pb::SetAlbumTypeRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let album_id = parse_uuid(&b.album_id, "album")?;
        let single_track_id = if b.single_track_id.trim().is_empty() {
            None
        } else {
            Some(parse_uuid(&b.single_track_id, "track")?)
        };
        let a = self
            .library
            .set_album_type(&caller, album_id, &b.album_type, single_track_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(album_to_pb(a)))
    }
    async fn list_artist_aliases(
        &self,
        req: Request<pb::GetArtistRequest>,
    ) -> Result<Response<pb::ListAliasesResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "artist")?;
        let aliases = self
            .library
            .list_artist_aliases(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListAliasesResponse {
            aliases: aliases.into_iter().map(artist_alias_to_pb).collect(),
        }))
    }
    async fn add_artist_alias(
        &self,
        req: Request<pb::AddArtistAliasRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.artist_id, "artist")?;
        let a = self
            .library
            .add_artist_alias(
                &caller,
                id,
                &b.name,
                nonempty(b.sort_name).as_deref(),
                nonempty(b.language).as_deref(),
            )
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn remove_artist_alias(
        &self,
        req: Request<pb::RemoveAliasRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "artist")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let a = self
            .library
            .remove_artist_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn set_primary_artist_alias(
        &self,
        req: Request<pb::SetPrimaryAliasRequest>,
    ) -> Result<Response<pb::Artist>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "artist")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let a = self
            .library
            .set_primary_artist_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.artist_pb_with_aliases(&caller, a).await?))
    }
    async fn list_album_aliases(
        &self,
        req: Request<pb::GetAlbumRequest>,
    ) -> Result<Response<pb::ListAliasesResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "album")?;
        let aliases = self
            .library
            .list_album_aliases(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListAliasesResponse {
            aliases: aliases.into_iter().map(album_alias_to_pb).collect(),
        }))
    }
    async fn add_album_alias(
        &self,
        req: Request<pb::AddAlbumAliasRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.album_id, "album")?;
        let a = self
            .library
            .add_album_alias(&caller, id, &b.title, nonempty(b.language).as_deref())
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }
    async fn remove_album_alias(
        &self,
        req: Request<pb::RemoveAliasRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "album")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let a = self
            .library
            .remove_album_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }
    async fn set_primary_album_alias(
        &self,
        req: Request<pb::SetPrimaryAliasRequest>,
    ) -> Result<Response<pb::Album>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "album")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let a = self
            .library
            .set_primary_album_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.album_pb_with_aliases(&caller, a).await?))
    }

    // ---- Track aliases ----
    async fn list_track_aliases(
        &self,
        req: Request<pb::GetTrackRequest>,
    ) -> Result<Response<pb::ListAliasesResponse>, Status> {
        let caller = self.caller(&req).await?;
        let id = parse_uuid(&req.into_inner().id, "track")?;
        let aliases = self
            .library
            .list_track_aliases(&caller, id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ListAliasesResponse {
            aliases: aliases.into_iter().map(track_alias_to_pb).collect(),
        }))
    }
    async fn add_track_alias(
        &self,
        req: Request<pb::AddTrackAliasRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.track_id, "track")?;
        let t = self
            .library
            .add_track_alias(&caller, id, &b.title, nonempty(b.language).as_deref())
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.track_pb_with_aliases(&caller, t).await?))
    }
    async fn remove_track_alias(
        &self,
        req: Request<pb::RemoveAliasRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "track")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let t = self
            .library
            .remove_track_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.track_pb_with_aliases(&caller, t).await?))
    }
    async fn set_primary_track_alias(
        &self,
        req: Request<pb::SetPrimaryAliasRequest>,
    ) -> Result<Response<pb::Track>, Status> {
        let caller = self.caller(&req).await?;
        let b = req.into_inner();
        let id = parse_uuid(&b.entity_id, "track")?;
        let alias_id = parse_uuid(&b.alias_id, "alias")?;
        let t = self
            .library
            .set_primary_track_alias(&caller, id, alias_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(self.track_pb_with_aliases(&caller, t).await?))
    }

    // ---- Scan ----
    async fn get_library_storage(
        &self,
        req: Request<pb::GetLibraryStorageRequest>,
    ) -> Result<Response<pb::LibraryStorage>, Status> {
        // Any authed identity may read the breakdown; resolving the caller
        // enforces a valid token.
        let _caller = self.caller(&req).await?;
        let s = self.storage.get_stats().await.map_err(map_err)?;
        Ok(Response::new(library_storage_to_pb(s)))
    }

    async fn scan_library(
        &self,
        req: Request<pb::ScanRequest>,
    ) -> Result<Response<pb::ScanResponse>, Status> {
        let caller = self.caller(&req).await?;
        let root = req.into_inner().root_path;
        let root_arg = if root.is_empty() { None } else { Some(PathBuf::from(root)) };
        let report = self
            .scan
            .scan(&caller, root_arg.as_deref())
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::ScanResponse {
            tracks_added: report.tracks_added as i64,
            tracks_skipped: report.tracks_skipped as i64,
            errors: report.errors as i64,
        }))
    }

    async fn rescan_library(
        &self,
        req: Request<pb::RescanRequest>,
    ) -> Result<Response<pb::RescanResponse>, Status> {
        let caller = self.caller(&req).await?;
        let full = req.into_inner().full_metadata;
        let report = self
            .scan
            .rescan_library(&caller, full)
            .await
            .map_err(map_err)?;
        Ok(Response::new(pb::RescanResponse {
            tracks_checked: report.total as i64,
            tracks_updated: report.corrected as i64,
            errors: report.errors as i64,
        }))
    }
}

// Keep `AppError` import alive even if some helpers below get pruned.
#[allow(dead_code)]
fn _force(_: AppError) {}
