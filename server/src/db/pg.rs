//! Postgres implementations of the repository traits.
//!
//! Queries are runtime-checked (`sqlx::query` / `query_as`) rather than the
//! `query!` macro so the crate builds without a live database. A future pass
//! can switch to compile-time checking via `cargo sqlx prepare` once the dev
//! DB is part of every contributor's workflow.

use async_trait::async_trait;
use sqlx::{FromRow, PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{AppError, Result};

use super::models::*;
use super::repo::*;

/// Wrap a `sqlx` error into [`AppError`].
fn db(e: sqlx::Error) -> AppError {
    AppError::Internal(format!("db error: {e}"))
}

// ---------------------------------------------------------------------------
// Shared handle: one `PgPool` clone per repo (cheap — `PgPool` is `Arc` inside).
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgRepos {
    pool: PgPool,
}

impl PgRepos {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ---------------------------------------------------------------------------
// UserRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl UserRepo for PgRepos {
    async fn create(&self, new: NewUser) -> Result<User> {
        sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (username, password_hash, permission_level)
            VALUES ($1, $2, $3)
            RETURNING id, username, password_hash, permission_level,
                      created_at, updated_at
            "#,
        )
        .bind(&new.username)
        .bind(&new.password_hash)
        .bind(new.permission_level)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<User>> {
        sqlx::query_as::<_, User>(
            r#"SELECT id, username, password_hash, permission_level,
                       created_at, updated_at
               FROM users WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<User>> {
        sqlx::query_as::<_, User>(
            r#"SELECT id, username, password_hash, permission_level,
                       created_at, updated_at
               FROM users WHERE username = $1"#,
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn update_permission(&self, id: Uuid, level: PermissionLevel) -> Result<()> {
        sqlx::query(
            r#"UPDATE users SET permission_level = $1, updated_at = now()
               WHERE id = $2"#,
        )
        .bind(level)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn update_password(&self, id: Uuid, password_hash: &str) -> Result<()> {
        sqlx::query(
            r#"UPDATE users SET password_hash = $1, updated_at = now()
               WHERE id = $2"#,
        )
        .bind(password_hash)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<User>> {
        sqlx::query_as::<_, User>(
            r#"SELECT id, username, password_hash, permission_level,
                       created_at, updated_at
               FROM users ORDER BY username"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ArtistRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl ArtistRepo for PgRepos {
    async fn create(&self, new: NewArtist) -> Result<Artist> {
        sqlx::query_as::<_, Artist>(
            r#"INSERT INTO artists (name, sort_name) VALUES ($1, $2)
               RETURNING id, name, sort_name, image_path, storage_bytes, created_at, updated_at"#,
        )
        .bind(&new.name)
        .bind(&new.sort_name)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, image_path, storage_bytes, created_at, updated_at
               FROM artists WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, image_path, storage_bytes, created_at, updated_at
               FROM artists ORDER BY name ASC LIMIT $1 OFFSET $2"#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM artists")
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
        Ok(n)
    }

    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Artist>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, image_path, storage_bytes, created_at, updated_at
               FROM artists
               WHERE name ILIKE $1 OR sort_name ILIKE $1
               ORDER BY name ASC
               LIMIT $2 OFFSET $3"#,
        )
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn update(&self, id: Uuid, name: &str, sort_name: Option<&str>) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"UPDATE artists
               SET name = $2, sort_name = $3, updated_at = now()
               WHERE id = $1
               RETURNING id, name, sort_name, image_path, storage_bytes, created_at, updated_at"#,
        )
        .bind(id)
        .bind(name)
        .bind(sort_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_image(&self, id: Uuid, image_path: Option<&str>) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"UPDATE artists
               SET image_path = $2, updated_at = now()
               WHERE id = $1
               RETURNING id, name, sort_name, image_path, storage_bytes, created_at, updated_at"#,
        )
        .bind(id)
        .bind(image_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn all_image_paths(&self) -> Result<Vec<(Uuid, String)>> {
        sqlx::query_as::<_, (Uuid, String)>(
            r#"SELECT id, image_path FROM artists
               WHERE image_path IS NOT NULL AND image_path <> ''"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, image_path, storage_bytes, created_at, updated_at
               FROM artists WHERE name = $1 LIMIT 1"#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AlbumRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl AlbumRepo for PgRepos {
    async fn create(&self, new: NewAlbum) -> Result<Album> {
        sqlx::query_as::<_, Album>(
            r#"INSERT INTO albums (artist_id, title, release_year, cover_path)
               VALUES ($1, $2, $3, $4)
               RETURNING id, artist_id, title, release_year, cover_path, storage_bytes,
                         created_at, updated_at"#,
        )
        .bind(new.artist_id)
        .bind(&new.title)
        .bind(new.release_year)
        .bind(&new.cover_path)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Album>> {
        sqlx::query_as::<_, Album>(
            r#"SELECT id, artist_id, title, release_year, cover_path, storage_bytes,
                       created_at, updated_at
               FROM albums WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_by_artist(&self, artist_id: Uuid) -> Result<Vec<Album>> {
        sqlx::query_as::<_, Album>(
            r#"SELECT id, artist_id, title, release_year, cover_path, storage_bytes,
                       created_at, updated_at
               FROM albums WHERE artist_id = $1
               ORDER BY release_year NULLS LAST, title"#,
        )
        .bind(artist_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Album>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        sqlx::query_as::<_, Album>(
            r#"SELECT id, artist_id, title, release_year, cover_path, storage_bytes,
                       created_at, updated_at
               FROM albums
               WHERE title ILIKE $1
               ORDER BY title
               LIMIT $2 OFFSET $3"#,
        )
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn update(
        &self,
        id: Uuid,
        title: &str,
        release_year: Option<i32>,
        cover_path: Option<&str>,
    ) -> Result<Option<Album>> {
        sqlx::query_as::<_, Album>(
            r#"UPDATE albums
               SET title = $2, release_year = $3, cover_path = $4, updated_at = now()
               WHERE id = $1
               RETURNING id, artist_id, title, release_year, cover_path, storage_bytes,
                         created_at, updated_at"#,
        )
        .bind(id)
        .bind(title)
        .bind(release_year)
        .bind(cover_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn find_by_artist_and_title(
        &self,
        artist_id: Uuid,
        title: &str,
    ) -> Result<Option<Album>> {
        sqlx::query_as::<_, Album>(
            r#"SELECT id, artist_id, title, release_year, cover_path, storage_bytes,
                       created_at, updated_at
               FROM albums WHERE artist_id = $1 AND title = $2 LIMIT 1"#,
        )
        .bind(artist_id)
        .bind(title)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn all_cover_paths(&self) -> Result<Vec<(Uuid, String)>> {
        sqlx::query_as::<_, (Uuid, String)>(
            r#"SELECT id, cover_path FROM albums
               WHERE cover_path IS NOT NULL AND cover_path <> ''"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<u64> {
        let res = sqlx::query("UPDATE albums SET artist_id = $2, updated_at = now() WHERE artist_id = $1")
            .bind(from_artist)
            .bind(to_artist)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(res.rows_affected())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM albums WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TrackRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl TrackRepo for PgRepos {
    async fn create(&self, new: NewTrack) -> Result<Track> {
        sqlx::query_as::<_, Track>(
            r#"INSERT INTO tracks
                 (album_id, artist_id, title, track_no, disc_no, duration_ms,
                  codec, bitrate_kbps, file_path, file_size,
                  sample_rate_hz, bit_depth, channels, metadata_json)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(new.album_id)
        .bind(new.artist_id)
        .bind(&new.title)
        .bind(new.track_no)
        .bind(new.disc_no)
        .bind(new.duration_ms)
        .bind(&new.codec)
        .bind(new.bitrate_kbps)
        .bind(&new.file_path)
        .bind(new.file_size)
        .bind(new.sample_rate_hz)
        .bind(new.bit_depth)
        .bind(new.channels)
        .bind(&new.metadata_json)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                       duration_ms, codec, bitrate_kbps, file_path, file_size,
                       sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at
               FROM tracks WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_by_album(&self, album_id: Uuid) -> Result<Vec<Track>> {
        sqlx::query_as::<_, Track>(
            r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                       duration_ms, codec, bitrate_kbps, file_path, file_size,
                       sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at
               FROM tracks WHERE album_id = $1
               ORDER BY disc_no NULLS FIRST, track_no NULLS LAST, title"#,
        )
        .bind(album_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Track>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        sqlx::query_as::<_, Track>(
            r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                       duration_ms, codec, bitrate_kbps, file_path, file_size,
                       sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at
               FROM tracks
               WHERE title ILIKE $1
               ORDER BY title
               LIMIT $2 OFFSET $3"#,
        )
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn update(
        &self,
        id: Uuid,
        title: &str,
        track_no: Option<i32>,
        disc_no: Option<i32>,
        metadata_json: &str,
    ) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"UPDATE tracks
               SET title = $2, track_no = $3, disc_no = $4, metadata_json = $5,
                   updated_at = now()
               WHERE id = $1
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(id)
        .bind(title)
        .bind(track_no)
        .bind(disc_no)
        .bind(metadata_json)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn find_by_file_path(&self, file_path: &str) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                       duration_ms, codec, bitrate_kbps, file_path, file_size,
                       sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at
               FROM tracks WHERE file_path = $1 LIMIT 1"#,
        )
        .bind(file_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM tracks WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn list_all_ids_paths(&self) -> Result<Vec<TrackIdPath>> {
        sqlx::query_as::<_, TrackIdPath>(
            "SELECT id, file_path, duration_ms FROM tracks ORDER BY file_path",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn update_duration(&self, id: Uuid, duration_ms: i64) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"UPDATE tracks
               SET duration_ms = $2, updated_at = now()
               WHERE id = $1
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(id)
        .bind(duration_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn update_file_props(
        &self,
        id: Uuid,
        codec: &str,
        bitrate_kbps: Option<i32>,
        file_size: Option<i64>,
        sample_rate_hz: Option<i32>,
        bit_depth: Option<i32>,
        channels: Option<i32>,
    ) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"UPDATE tracks
               SET codec = $2, bitrate_kbps = $3, file_size = $4,
                   sample_rate_hz = $5, bit_depth = $6, channels = $7,
                   updated_at = now()
               WHERE id = $1
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(id)
        .bind(codec)
        .bind(bitrate_kbps)
        .bind(file_size)
        .bind(sample_rate_hz)
        .bind(bit_depth)
        .bind(channels)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<u64> {
        let res = sqlx::query("UPDATE tracks SET artist_id = $2, updated_at = now() WHERE artist_id = $1")
            .bind(from_artist)
            .bind(to_artist)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(res.rows_affected())
    }

    async fn reassign_album(&self, from_album: Uuid, to_album: Uuid) -> Result<u64> {
        let res = sqlx::query("UPDATE tracks SET album_id = $2, updated_at = now() WHERE album_id = $1")
            .bind(from_album)
            .bind(to_album)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(res.rows_affected())
    }

    async fn set_album(&self, id: Uuid, album_id: Uuid) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"UPDATE tracks
               SET album_id = $2, updated_at = now()
               WHERE id = $1
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(id)
        .bind(album_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_single_release(&self, id: Uuid, is_single_release: bool) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"UPDATE tracks
               SET is_single_release = $2, updated_at = now()
               WHERE id = $1
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         sample_rate_hz, bit_depth, channels, metadata_json,
                         is_single_release, created_at, updated_at"#,
        )
        .bind(id)
        .bind(is_single_release)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }
}

// ---------------------------------------------------------------------------
// PlaylistRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl PlaylistRepo for PgRepos {
    async fn create(&self, new: NewPlaylist) -> Result<Playlist> {
        sqlx::query_as::<_, Playlist>(
            r#"INSERT INTO playlists (owner_id, name) VALUES ($1, $2)
               RETURNING id, owner_id, name, created_at, updated_at"#,
        )
        .bind(new.owner_id)
        .bind(&new.name)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Playlist>> {
        sqlx::query_as::<_, Playlist>(
            r#"SELECT id, owner_id, name, created_at, updated_at
               FROM playlists WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_for_owner(&self, owner_id: Uuid) -> Result<Vec<Playlist>> {
        sqlx::query_as::<_, Playlist>(
            r#"SELECT id, owner_id, name, created_at, updated_at
               FROM playlists WHERE owner_id = $1
               ORDER BY created_at DESC"#,
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn update_name(&self, id: Uuid, name: &str) -> Result<Option<Playlist>> {
        sqlx::query_as::<_, Playlist>(
            r#"UPDATE playlists
               SET name = $2, updated_at = now()
               WHERE id = $1
               RETURNING id, owner_id, name, created_at, updated_at"#,
        )
        .bind(id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM playlists WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn insert_track_at(
        &self,
        playlist_id: Uuid,
        track_id: Uuid,
        position: i32,
    ) -> Result<PlaylistTrack> {
        // Two-step shift to avoid PK collisions on (playlist_id, position):
        //   1. Move every row at >= position into the negative space.
        //   2. Insert the new row at the requested position.
        //   3. Move the shifted rows back, one slot higher than before.
        let mut tx = self.pool.begin().await.map_err(db)?;

        sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = -(position + 1)
               WHERE playlist_id = $1 AND position >= $2"#,
        )
        .bind(playlist_id)
        .bind(position)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        let row = sqlx::query_as::<_, PlaylistTrack>(
            r#"INSERT INTO playlist_tracks (playlist_id, track_id, position)
               VALUES ($1, $2, $3)
               RETURNING playlist_id, track_id, position, added_at"#,
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(position)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;

        sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = -position
               WHERE playlist_id = $1 AND position < 0"#,
        )
        .bind(playlist_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        tx.commit().await.map_err(db)?;
        Ok(row)
    }

    async fn remove_track_at(&self, playlist_id: Uuid, position: i32) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;

        let res = sqlx::query(
            r#"DELETE FROM playlist_tracks
               WHERE playlist_id = $1 AND position = $2"#,
        )
        .bind(playlist_id)
        .bind(position)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        if res.rows_affected() == 0 {
            tx.rollback().await.map_err(db)?;
            return Ok(false);
        }

        // Shift later rows down by one, two-step to avoid PK collisions.
        sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = -(position - 1)
               WHERE playlist_id = $1 AND position > $2"#,
        )
        .bind(playlist_id)
        .bind(position)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = -position
               WHERE playlist_id = $1 AND position < 0"#,
        )
        .bind(playlist_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    async fn move_track(&self, playlist_id: Uuid, from: i32, to: i32) -> Result<bool> {
        if from == to {
            // Confirm the row exists so the service layer can decide 404 vs no-op.
            let exists: Option<(i32,)> = sqlx::query_as(
                r#"SELECT position FROM playlist_tracks
                   WHERE playlist_id = $1 AND position = $2"#,
            )
            .bind(playlist_id)
            .bind(from)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
            return Ok(exists.is_some());
        }

        let mut tx = self.pool.begin().await.map_err(db)?;

        // Park the moving row in the negative space so the shift can sweep
        // the in-between range without colliding with it.
        let parked = sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = -1
               WHERE playlist_id = $1 AND position = $2"#,
        )
        .bind(playlist_id)
        .bind(from)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        if parked.rows_affected() == 0 {
            tx.rollback().await.map_err(db)?;
            return Ok(false);
        }

        if from < to {
            // Slide [from+1 ..= to] down by one.
            sqlx::query(
                r#"UPDATE playlist_tracks
                   SET position = -(position - 1 + 1000000)
                   WHERE playlist_id = $1 AND position > $2 AND position <= $3"#,
            )
            .bind(playlist_id)
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            sqlx::query(
                r#"UPDATE playlist_tracks
                   SET position = -position - 1000000
                   WHERE playlist_id = $1 AND position < -1"#,
            )
            .bind(playlist_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        } else {
            // from > to: slide [to ..= from-1] up by one.
            sqlx::query(
                r#"UPDATE playlist_tracks
                   SET position = -(position + 1 + 1000000)
                   WHERE playlist_id = $1 AND position >= $2 AND position < $3"#,
            )
            .bind(playlist_id)
            .bind(to)
            .bind(from)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            sqlx::query(
                r#"UPDATE playlist_tracks
                   SET position = -position - 1000000
                   WHERE playlist_id = $1 AND position < -1"#,
            )
            .bind(playlist_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }

        sqlx::query(
            r#"UPDATE playlist_tracks
               SET position = $2
               WHERE playlist_id = $1 AND position = -1"#,
        )
        .bind(playlist_id)
        .bind(to)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    async fn list_tracks(&self, playlist_id: Uuid) -> Result<Vec<PlaylistTrack>> {
        sqlx::query_as::<_, PlaylistTrack>(
            r#"SELECT playlist_id, track_id, position, added_at
               FROM playlist_tracks
               WHERE playlist_id = $1
               ORDER BY position"#,
        )
        .bind(playlist_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn next_position(&self, playlist_id: Uuid) -> Result<i32> {
        let (n,): (Option<i32>,) = sqlx::query_as(
            r#"SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = $1"#,
        )
        .bind(playlist_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n.map(|m| m + 1).unwrap_or(1))
    }

    async fn get_track_at(
        &self,
        playlist_id: Uuid,
        position: i32,
    ) -> Result<Option<PlaylistTrack>> {
        sqlx::query_as::<_, PlaylistTrack>(
            r#"SELECT playlist_id, track_id, position, added_at
               FROM playlist_tracks
               WHERE playlist_id = $1 AND position = $2"#,
        )
        .bind(playlist_id)
        .bind(position)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }
}

// ---------------------------------------------------------------------------
// FollowRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl FollowRepo for PgRepos {
    async fn follow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO follows (user_id, artist_id) VALUES ($1, $2)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(user_id)
        .bind(artist_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn unfollow(&self, user_id: Uuid, artist_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM follows WHERE user_id = $1 AND artist_id = $2")
            .bind(user_id)
            .bind(artist_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn followers_of(&self, artist_id: Uuid) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT user_id FROM follows WHERE artist_id = $1")
                .bind(artist_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows.into_iter().map(|(u,)| u).collect())
    }

    async fn following(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT artist_id FROM follows WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows.into_iter().map(|(a,)| a).collect())
    }

    async fn reassign_artist(&self, from_artist: Uuid, to_artist: Uuid) -> Result<()> {
        // Re-point follows, skipping users who already follow the survivor
        // (the `(user_id, artist_id)` PK would collide), then drop the
        // now-redundant rows still pointing at the merged-away artist.
        sqlx::query(
            r#"UPDATE follows SET artist_id = $2
               WHERE artist_id = $1
                 AND user_id NOT IN (SELECT user_id FROM follows WHERE artist_id = $2)"#,
        )
        .bind(from_artist)
        .bind(to_artist)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM follows WHERE artist_id = $1")
            .bind(from_artist)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AliasRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl AliasRepo for PgRepos {
    async fn list_artist_aliases(&self, artist_id: Uuid) -> Result<Vec<ArtistAlias>> {
        sqlx::query_as::<_, ArtistAlias>(
            r#"SELECT id, artist_id, name, sort_name, language, is_primary, created_at
               FROM artist_aliases WHERE artist_id = $1
               ORDER BY is_primary DESC, created_at"#,
        )
        .bind(artist_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn add_artist_alias(&self, new: NewArtistAlias) -> Result<ArtistAlias> {
        // Idempotent on (artist_id, name): a conflict returns the existing row
        // unchanged (a no-op update lets RETURNING fire on conflict too).
        sqlx::query_as::<_, ArtistAlias>(
            r#"INSERT INTO artist_aliases (artist_id, name, sort_name, language, is_primary)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (artist_id, name)
               DO UPDATE SET name = artist_aliases.name
               RETURNING id, artist_id, name, sort_name, language, is_primary, created_at"#,
        )
        .bind(new.artist_id)
        .bind(&new.name)
        .bind(&new.sort_name)
        .bind(&new.language)
        .bind(new.is_primary)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_artist_alias(&self, id: Uuid) -> Result<Option<ArtistAlias>> {
        sqlx::query_as::<_, ArtistAlias>(
            r#"SELECT id, artist_id, name, sort_name, language, is_primary, created_at
               FROM artist_aliases WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete_artist_alias(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM artist_aliases WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn set_primary_artist_alias(&self, artist_id: Uuid, alias_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"UPDATE artist_aliases
               SET is_primary = (id = $2)
               WHERE artist_id = $1"#,
        )
        .bind(artist_id)
        .bind(alias_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn reassign_artist_aliases(&self, from_artist: Uuid, to_artist: Uuid) -> Result<()> {
        // Move aliases that don't already exist on the survivor; reassigned
        // rows lose their primary flag (the survivor keeps its own primary).
        sqlx::query(
            r#"UPDATE artist_aliases
               SET artist_id = $2, is_primary = false
               WHERE artist_id = $1
                 AND name NOT IN (SELECT name FROM artist_aliases WHERE artist_id = $2)"#,
        )
        .bind(from_artist)
        .bind(to_artist)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        // Drop any leftover (duplicate-name) aliases still on the source; they
        // cascade anyway when the source artist is deleted, but clear them now
        // so the source row count is accurate if the caller inspects it.
        sqlx::query("DELETE FROM artist_aliases WHERE artist_id = $1")
            .bind(from_artist)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn list_album_aliases(&self, album_id: Uuid) -> Result<Vec<AlbumAlias>> {
        sqlx::query_as::<_, AlbumAlias>(
            r#"SELECT id, album_id, title, language, is_primary, created_at
               FROM album_aliases WHERE album_id = $1
               ORDER BY is_primary DESC, created_at"#,
        )
        .bind(album_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn add_album_alias(&self, new: NewAlbumAlias) -> Result<AlbumAlias> {
        sqlx::query_as::<_, AlbumAlias>(
            r#"INSERT INTO album_aliases (album_id, title, language, is_primary)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (album_id, title)
               DO UPDATE SET title = album_aliases.title
               RETURNING id, album_id, title, language, is_primary, created_at"#,
        )
        .bind(new.album_id)
        .bind(&new.title)
        .bind(&new.language)
        .bind(new.is_primary)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_album_alias(&self, id: Uuid) -> Result<Option<AlbumAlias>> {
        sqlx::query_as::<_, AlbumAlias>(
            r#"SELECT id, album_id, title, language, is_primary, created_at
               FROM album_aliases WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete_album_alias(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM album_aliases WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn set_primary_album_alias(&self, album_id: Uuid, alias_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"UPDATE album_aliases
               SET is_primary = (id = $2)
               WHERE album_id = $1"#,
        )
        .bind(album_id)
        .bind(alias_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn reassign_album_aliases(&self, from_album: Uuid, to_album: Uuid) -> Result<()> {
        sqlx::query(
            r#"UPDATE album_aliases
               SET album_id = $2, is_primary = false
               WHERE album_id = $1
                 AND title NOT IN (SELECT title FROM album_aliases WHERE album_id = $2)"#,
        )
        .bind(from_album)
        .bind(to_album)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM album_aliases WHERE album_id = $1")
            .bind(from_album)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DeviceTokenRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl DeviceTokenRepo for PgRepos {
    async fn upsert(&self, new: NewDeviceToken) -> Result<DeviceToken> {
        // On a token conflict, re-own it (the device re-logged-in as another
        // user) + bump last_seen_at.
        sqlx::query_as::<_, DeviceToken>(
            r#"INSERT INTO device_tokens (token, user_id, platform)
               VALUES ($1, $2, $3)
               ON CONFLICT (token) DO UPDATE
                 SET user_id = EXCLUDED.user_id,
                     platform = EXCLUDED.platform,
                     last_seen_at = now()
               RETURNING token, user_id, platform, created_at, last_seen_at"#,
        )
        .bind(&new.token)
        .bind(new.user_id)
        .bind(&new.platform)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_for_user(&self, user_id: Uuid) -> Result<Vec<DeviceToken>> {
        sqlx::query_as::<_, DeviceToken>(
            r#"SELECT token, user_id, platform, created_at, last_seen_at
               FROM device_tokens WHERE user_id = $1"#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, token: &str) -> Result<()> {
        sqlx::query("DELETE FROM device_tokens WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AuditRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl AuditRepo for PgRepos {
    async fn record(&self, entry: NewAuditEntry) -> Result<AuditEntry> {
        sqlx::query_as::<_, AuditEntry>(
            r#"INSERT INTO audit_log
                 (actor_id, action, entity_type, entity_id, before_json, after_json)
               VALUES ($1, $2, $3, $4, $5, $6)
               RETURNING id, actor_id, action, entity_type, entity_id,
                         before_json, after_json, created_at"#,
        )
        .bind(entry.actor_id)
        .bind(&entry.action)
        .bind(&entry.entity_type)
        .bind(entry.entity_id)
        .bind(&entry.before_json)
        .bind(&entry.after_json)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_for_entity(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> Result<Vec<AuditEntry>> {
        sqlx::query_as::<_, AuditEntry>(
            r#"SELECT id, actor_id, action, entity_type, entity_id,
                       before_json, after_json, created_at
               FROM audit_log
               WHERE entity_type = $1 AND entity_id = $2
               ORDER BY created_at DESC"#,
        )
        .bind(entity_type)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }
}

// ---------------------------------------------------------------------------
// NotificationRepo
// ---------------------------------------------------------------------------

const NOTIFICATION_COLS: &str =
    "id, user_id, kind, artist_id, album_id, podcast_id, episode_id, title, body, read_at, created_at";

#[async_trait]
impl NotificationRepo for PgRepos {
    async fn create(&self, new: NewNotification) -> Result<Notification> {
        sqlx::query_as::<_, Notification>(&format!(
            "INSERT INTO notifications \
               (user_id, kind, artist_id, album_id, podcast_id, episode_id, title, body) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {NOTIFICATION_COLS}"
        ))
        .bind(new.user_id)
        .bind(&new.kind)
        .bind(new.artist_id)
        .bind(new.album_id)
        .bind(new.podcast_id)
        .bind(new.episode_id)
        .bind(&new.title)
        .bind(&new.body)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn create_many(&self, items: &[NewNotification]) -> Result<u64> {
        if items.is_empty() {
            return Ok(0);
        }
        // One multi-row INSERT (single round-trip) for the new-release /
        // new-episode fan-out.
        let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO notifications \
               (user_id, kind, artist_id, album_id, podcast_id, episode_id, title, body) ",
        );
        qb.push_values(items, |mut b, item| {
            b.push_bind(item.user_id)
                .push_bind(&item.kind)
                .push_bind(item.artist_id)
                .push_bind(item.album_id)
                .push_bind(item.podcast_id)
                .push_bind(item.episode_id)
                .push_bind(&item.title)
                .push_bind(&item.body);
        });
        let res = qb.build().execute(&self.pool).await.map_err(db)?;
        Ok(res.rows_affected())
    }

    async fn get(&self, id: Uuid) -> Result<Option<Notification>> {
        sqlx::query_as::<_, Notification>(&format!(
            "SELECT {NOTIFICATION_COLS} FROM notifications WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Notification>> {
        sqlx::query_as::<_, Notification>(&format!(
            "SELECT {NOTIFICATION_COLS} FROM notifications \
             WHERE user_id = $1 AND ($2 = FALSE OR read_at IS NULL) \
             ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ))
        .bind(user_id)
        .bind(unread_only)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn unread_count(&self, user_id: Uuid) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n)
    }

    async fn mark_read(&self, user_id: Uuid, id: Uuid) -> Result<bool> {
        // Guard on `read_at IS NULL` so a re-read keeps the original timestamp;
        // scope on user_id so a caller can never flip another user's row.
        let res = sqlx::query(
            "UPDATE notifications SET read_at = now() \
             WHERE id = $1 AND user_id = $2 AND read_at IS NULL",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(res.rows_affected() > 0)
    }

    async fn mark_all_read(&self, user_id: Uuid) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE notifications SET read_at = now() \
             WHERE user_id = $1 AND read_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(res.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// SessionRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionRepo for PgRepos {
    async fn create(&self, new: NewSession) -> Result<Session> {
        sqlx::query_as::<_, Session>(
            r#"INSERT INTO sessions (token, user_id, expires_at)
               VALUES ($1, $2, $3)
               RETURNING token, user_id, created_at, expires_at, revoked_at"#,
        )
        .bind(&new.token)
        .bind(new.user_id)
        .bind(new.expires_at)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, token: &str) -> Result<Option<Session>> {
        sqlx::query_as::<_, Session>(
            r#"SELECT token, user_id, created_at, expires_at, revoked_at
               FROM sessions WHERE token = $1"#,
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn revoke(&self, token: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET revoked_at = now() WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// UploadRepo
// ---------------------------------------------------------------------------

const UPLOAD_COLS: &str =
    "id, user_id, state, total_files, total_bytes, report_json, error, created_at, updated_at";
const UPLOAD_FILE_COLS: &str =
    "id, upload_id, file_index, filename, file_hash, total_size, chunk_size, \
     total_chunks, received_chunks, state, error, created_at";
const UPLOAD_CHUNK_COLS: &str =
    "upload_file_id, chunk_index, start_byte, end_byte, hash, received, received_at";

#[async_trait]
impl UploadRepo for PgRepos {
    async fn create_upload(&self, new: NewUpload) -> Result<Upload> {
        sqlx::query_as::<_, Upload>(&format!(
            "INSERT INTO uploads (user_id, total_files, total_bytes) \
             VALUES ($1, $2, $3) RETURNING {UPLOAD_COLS}"
        ))
        .bind(new.user_id)
        .bind(new.total_files)
        .bind(new.total_bytes)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_upload(&self, id: Uuid) -> Result<Option<Upload>> {
        sqlx::query_as::<_, Upload>(&format!("SELECT {UPLOAD_COLS} FROM uploads WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)
    }

    async fn list_uploads(
        &self,
        filter: UploadFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Upload>> {
        sqlx::query_as::<_, Upload>(&format!(
            "SELECT {UPLOAD_COLS} FROM uploads \
             WHERE ($1::uuid IS NULL OR user_id = $1) \
               AND ($2::text IS NULL OR state = $2) \
             ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ))
        .bind(filter.user_id)
        .bind(filter.state)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn count_active_for_user(&self, user_id: Option<Uuid>) -> Result<i64> {
        // `IS NOT DISTINCT FROM` so a NULL owner (SECRET_KEY) matches NULL rows.
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM uploads \
             WHERE state IN ('initialized', 'uploading', 'paused') \
               AND user_id IS NOT DISTINCT FROM $1",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n)
    }

    async fn set_upload_state(&self, id: Uuid, state: UploadState) -> Result<()> {
        sqlx::query("UPDATE uploads SET state = $1, updated_at = now() WHERE id = $2")
            .bind(state)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn pause_stale_active(&self, cutoff: OffsetDateTime) -> Result<Vec<Upload>> {
        // Staleness = time since the most recent chunk receipt (or creation when
        // none) — but bounded below by the row's own `updated_at`, so a just-
        // resumed session (which bumps `updated_at`) isn't re-paused before its
        // first new chunk lands. `MAX` ignores the NULL `received_at` of
        // not-yet-received chunks. Atomic: only rows still active at update time
        // are paused + returned.
        sqlx::query_as::<_, Upload>(&format!(
            "UPDATE uploads u SET state = 'paused', updated_at = now() \
             WHERE u.state IN ('initialized', 'uploading') \
               AND GREATEST( \
                     u.updated_at, \
                     COALESCE( \
                       (SELECT MAX(c.received_at) FROM upload_chunks c \
                        JOIN upload_files f ON c.upload_file_id = f.id \
                        WHERE f.upload_id = u.id), \
                       u.created_at) \
                   ) < $1 \
             RETURNING {UPLOAD_COLS}"
        ))
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_upload_report(
        &self,
        id: Uuid,
        state: UploadState,
        report_json: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE uploads SET state = $1, report_json = $2, error = $3, updated_at = now() \
             WHERE id = $4",
        )
        .bind(state)
        .bind(report_json)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn create_file(&self, new: NewUploadFile) -> Result<UploadFile> {
        sqlx::query_as::<_, UploadFile>(&format!(
            "INSERT INTO upload_files \
             (upload_id, file_index, filename, file_hash, total_size, chunk_size, total_chunks) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {UPLOAD_FILE_COLS}"
        ))
        .bind(new.upload_id)
        .bind(new.file_index)
        .bind(&new.filename)
        .bind(&new.file_hash)
        .bind(new.total_size)
        .bind(new.chunk_size)
        .bind(new.total_chunks)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_file(&self, upload_id: Uuid, file_index: i32) -> Result<Option<UploadFile>> {
        sqlx::query_as::<_, UploadFile>(&format!(
            "SELECT {UPLOAD_FILE_COLS} FROM upload_files \
             WHERE upload_id = $1 AND file_index = $2"
        ))
        .bind(upload_id)
        .bind(file_index)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_files(&self, upload_id: Uuid) -> Result<Vec<UploadFile>> {
        sqlx::query_as::<_, UploadFile>(&format!(
            "SELECT {UPLOAD_FILE_COLS} FROM upload_files \
             WHERE upload_id = $1 ORDER BY file_index ASC"
        ))
        .bind(upload_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_file_state(
        &self,
        file_id: Uuid,
        state: UploadFileState,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE upload_files SET state = $1, error = $2 WHERE id = $3")
            .bind(state)
            .bind(error)
            .bind(file_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn set_file_filename(&self, file_id: Uuid, filename: &str) -> Result<()> {
        sqlx::query("UPDATE upload_files SET filename = $1 WHERE id = $2")
            .bind(filename)
            .bind(file_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn create_chunk(&self, new: NewUploadChunk) -> Result<()> {
        sqlx::query(
            "INSERT INTO upload_chunks \
             (upload_file_id, chunk_index, start_byte, end_byte, hash) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(new.upload_file_id)
        .bind(new.chunk_index)
        .bind(new.start_byte)
        .bind(new.end_byte)
        .bind(&new.hash)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn list_chunks(&self, file_id: Uuid) -> Result<Vec<UploadChunk>> {
        sqlx::query_as::<_, UploadChunk>(&format!(
            "SELECT {UPLOAD_CHUNK_COLS} FROM upload_chunks \
             WHERE upload_file_id = $1 ORDER BY chunk_index ASC"
        ))
        .bind(file_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_chunk(&self, file_id: Uuid, chunk_index: i32) -> Result<Option<UploadChunk>> {
        sqlx::query_as::<_, UploadChunk>(&format!(
            "SELECT {UPLOAD_CHUNK_COLS} FROM upload_chunks \
             WHERE upload_file_id = $1 AND chunk_index = $2"
        ))
        .bind(file_id)
        .bind(chunk_index)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn mark_chunk_received(&self, file_id: Uuid, chunk_index: i32) -> Result<(i32, i32)> {
        // Idempotent: only flips false→true so a retried chunk doesn't reset
        // received_at; the recompute below is correct regardless.
        sqlx::query(
            "UPDATE upload_chunks SET received = TRUE, received_at = now() \
             WHERE upload_file_id = $1 AND chunk_index = $2 AND received = FALSE",
        )
        .bind(file_id)
        .bind(chunk_index)
        .execute(&self.pool)
        .await
        .map_err(db)?;

        sqlx::query_as::<_, (i32, i32)>(
            "UPDATE upload_files \
             SET received_chunks = (SELECT COUNT(*) FROM upload_chunks \
                                    WHERE upload_file_id = $1 AND received), \
                 state = CASE WHEN state = 'pending' THEN 'uploading' ELSE state END \
             WHERE id = $1 RETURNING received_chunks, total_chunks",
        )
        .bind(file_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }
}

// ---------------------------------------------------------------------------
// PodcastRepo
// ---------------------------------------------------------------------------

const PODCAST_COLS: &str = "id, feed_url, title, author, description, image_path, image_url, \
     link, language, categories, itunes_id, podcastindex_id, auto_download, storage_bytes, \
     last_refreshed_at, last_etag, last_modified, created_at, updated_at";

#[async_trait]
impl PodcastRepo for PgRepos {
    async fn upsert_by_feed_url(&self, new: NewPodcast) -> Result<Podcast> {
        // The conflict path refreshes the feed-derived metadata only — it never
        // clobbers `image_path` (cached separately), `auto_download` (set by the
        // manager), or the refresh bookkeeping (`last_*`).
        sqlx::query_as::<_, Podcast>(&format!(
            "INSERT INTO podcasts \
               (feed_url, title, author, description, image_url, link, language, \
                categories, itunes_id, podcastindex_id, auto_download) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (feed_url) DO UPDATE SET \
               title = EXCLUDED.title, author = EXCLUDED.author, \
               description = EXCLUDED.description, image_url = EXCLUDED.image_url, \
               link = EXCLUDED.link, language = EXCLUDED.language, \
               categories = EXCLUDED.categories, itunes_id = EXCLUDED.itunes_id, \
               podcastindex_id = EXCLUDED.podcastindex_id, updated_at = now() \
             RETURNING {PODCAST_COLS}"
        ))
        .bind(&new.feed_url)
        .bind(&new.title)
        .bind(&new.author)
        .bind(&new.description)
        .bind(&new.image_url)
        .bind(&new.link)
        .bind(&new.language)
        .bind(&new.categories)
        .bind(new.itunes_id)
        .bind(new.podcastindex_id)
        .bind(new.auto_download)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "SELECT {PODCAST_COLS} FROM podcasts WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn get_by_feed_url(&self, feed_url: &str) -> Result<Option<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "SELECT {PODCAST_COLS} FROM podcasts WHERE feed_url = $1"
        ))
        .bind(feed_url)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "SELECT {PODCAST_COLS} FROM podcasts ORDER BY title ASC LIMIT $1 OFFSET $2"
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM podcasts")
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
        Ok(n)
    }

    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<Podcast>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        sqlx::query_as::<_, Podcast>(&format!(
            "SELECT {PODCAST_COLS} FROM podcasts \
             WHERE title ILIKE $1 OR author ILIKE $1 \
             ORDER BY title ASC LIMIT $2 OFFSET $3"
        ))
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_image(&self, id: Uuid, image_path: Option<&str>) -> Result<Option<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "UPDATE podcasts SET image_path = $2, updated_at = now() \
             WHERE id = $1 RETURNING {PODCAST_COLS}"
        ))
        .bind(id)
        .bind(image_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_auto_download(&self, id: Uuid, n: i32) -> Result<Option<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "UPDATE podcasts SET auto_download = $2, updated_at = now() \
             WHERE id = $1 RETURNING {PODCAST_COLS}"
        ))
        .bind(id)
        .bind(n)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn touch_refreshed(
        &self,
        id: Uuid,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE podcasts \
             SET last_refreshed_at = now(), last_etag = $2, last_modified = $3, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(etag)
        .bind(last_modified)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn all_for_refresh(&self) -> Result<Vec<Podcast>> {
        sqlx::query_as::<_, Podcast>(&format!(
            "SELECT {PODCAST_COLS} FROM podcasts ORDER BY last_refreshed_at ASC NULLS FIRST"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM podcasts WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PodcastEpisodeRepo
// ---------------------------------------------------------------------------

const EPISODE_COLS: &str = "id, podcast_id, guid, title, description, enclosure_url, \
     enclosure_type, episode_no, season_no, duration_ms, codec, bitrate_kbps, \
     file_path, file_size, image_path, published_at, metadata_json, created_at, updated_at";

#[async_trait]
impl PodcastEpisodeRepo for PgRepos {
    async fn upsert_by_guid(&self, new: NewPodcastEpisode) -> Result<(PodcastEpisode, bool)> {
        // `(xmax = 0)` is Postgres's "was this row just INSERTed?" signal — true
        // for a fresh insert, false on the conflict-UPDATE path. That's how the
        // refresh distinguishes a genuinely-new episode (→ fan out) from a
        // re-parse of an existing one. A downloaded episode keeps its measured
        // duration (file_path IS NOT NULL); file/codec/bitrate are never touched
        // here (only `set_file` writes them).
        let row = sqlx::query(&format!(
            "INSERT INTO podcast_episodes \
               (podcast_id, guid, title, description, enclosure_url, enclosure_type, \
                episode_no, season_no, duration_ms, image_path, published_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (podcast_id, guid) DO UPDATE SET \
               title = EXCLUDED.title, description = EXCLUDED.description, \
               enclosure_url = EXCLUDED.enclosure_url, enclosure_type = EXCLUDED.enclosure_type, \
               episode_no = EXCLUDED.episode_no, season_no = EXCLUDED.season_no, \
               duration_ms = CASE WHEN podcast_episodes.file_path IS NOT NULL \
                                  THEN podcast_episodes.duration_ms ELSE EXCLUDED.duration_ms END, \
               image_path = EXCLUDED.image_path, published_at = EXCLUDED.published_at, \
               updated_at = now() \
             RETURNING {EPISODE_COLS}, (xmax = 0) AS inserted"
        ))
        .bind(new.podcast_id)
        .bind(&new.guid)
        .bind(&new.title)
        .bind(&new.description)
        .bind(&new.enclosure_url)
        .bind(&new.enclosure_type)
        .bind(new.episode_no)
        .bind(new.season_no)
        .bind(new.duration_ms)
        .bind(&new.image_path)
        .bind(new.published_at)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        let inserted: bool = row.try_get("inserted").map_err(db)?;
        let ep = PodcastEpisode::from_row(&row).map_err(db)?;
        Ok((ep, inserted))
    }

    async fn get(&self, id: Uuid) -> Result<Option<PodcastEpisode>> {
        sqlx::query_as::<_, PodcastEpisode>(&format!(
            "SELECT {EPISODE_COLS} FROM podcast_episodes WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list_for_podcast(
        &self,
        podcast_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PodcastEpisode>> {
        sqlx::query_as::<_, PodcastEpisode>(&format!(
            "SELECT {EPISODE_COLS} FROM podcast_episodes WHERE podcast_id = $1 \
             ORDER BY published_at DESC NULLS LAST LIMIT $2 OFFSET $3"
        ))
        .bind(podcast_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn newest_undownloaded(
        &self,
        podcast_id: Uuid,
        limit: i64,
    ) -> Result<Vec<PodcastEpisode>> {
        sqlx::query_as::<_, PodcastEpisode>(&format!(
            "SELECT {EPISODE_COLS} FROM podcast_episodes \
             WHERE podcast_id = $1 AND file_path IS NULL \
             ORDER BY published_at DESC NULLS LAST LIMIT $2"
        ))
        .bind(podcast_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn all_guids(&self, podcast_id: Uuid) -> Result<Vec<String>> {
        sqlx::query_scalar::<_, String>("SELECT guid FROM podcast_episodes WHERE podcast_id = $1")
            .bind(podcast_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db)
    }

    async fn delete_stale_metadata(&self, podcast_id: Uuid, keep: &[String]) -> Result<u64> {
        // Remove only not-yet-downloaded rows; a downloaded episode keeps its
        // row (and its on-disk audio) even after it falls out of the feed.
        let res = sqlx::query(
            "DELETE FROM podcast_episodes \
             WHERE podcast_id = $1 AND file_path IS NULL AND guid <> ALL($2)",
        )
        .bind(podcast_id)
        .bind(keep)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(res.rows_affected())
    }

    async fn set_file(
        &self,
        id: Uuid,
        file_path: &str,
        file_size: Option<i64>,
        codec: Option<&str>,
        bitrate_kbps: Option<i32>,
        duration_ms: Option<i64>,
    ) -> Result<Option<PodcastEpisode>> {
        // Keep the existing (feed) duration when the probe couldn't measure one.
        sqlx::query_as::<_, PodcastEpisode>(&format!(
            "UPDATE podcast_episodes SET \
               file_path = $2, file_size = $3, codec = $4, bitrate_kbps = $5, \
               duration_ms = COALESCE($6, duration_ms), updated_at = now() \
             WHERE id = $1 RETURNING {EPISODE_COLS}"
        ))
        .bind(id)
        .bind(file_path)
        .bind(file_size)
        .bind(codec)
        .bind(bitrate_kbps)
        .bind(duration_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn clear_file(&self, id: Uuid) -> Result<Option<PodcastEpisode>> {
        sqlx::query_as::<_, PodcastEpisode>(&format!(
            "UPDATE podcast_episodes SET file_path = NULL, file_size = NULL, updated_at = now() \
             WHERE id = $1 RETURNING {EPISODE_COLS}"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM podcast_episodes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PodcastSubscriptionRepo (mirrors FollowRepo)
// ---------------------------------------------------------------------------

#[async_trait]
impl PodcastSubscriptionRepo for PgRepos {
    async fn subscribe(&self, user_id: Uuid, podcast_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO podcast_subscriptions (user_id, podcast_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(podcast_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn unsubscribe(&self, user_id: Uuid, podcast_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM podcast_subscriptions WHERE user_id = $1 AND podcast_id = $2")
            .bind(user_id)
            .bind(podcast_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn subscribers_of(&self, podcast_id: Uuid) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT user_id FROM podcast_subscriptions WHERE podcast_id = $1")
                .bind(podcast_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows.into_iter().map(|(u,)| u).collect())
    }

    async fn subscriptions(&self, user_id: Uuid) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT podcast_id FROM podcast_subscriptions WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows.into_iter().map(|(p,)| p).collect())
    }
}

// ---------------------------------------------------------------------------
// StorageRepo
// ---------------------------------------------------------------------------

#[async_trait]
impl StorageRepo for PgRepos {
    async fn recompute_entity_storage(&self) -> Result<()> {
        // Three independent rollups. `COALESCE(SUM(...), 0)` so an entity with
        // no (or all-NULL-size) files goes to 0 rather than NULL.
        sqlx::query(
            r#"UPDATE albums a
               SET storage_bytes = COALESCE(
                   (SELECT SUM(t.file_size) FROM tracks t WHERE t.album_id = a.id), 0)"#,
        )
        .execute(&self.pool)
        .await
        .map_err(db)?;
        sqlx::query(
            r#"UPDATE artists ar
               SET storage_bytes = COALESCE(
                   (SELECT SUM(t.file_size) FROM tracks t WHERE t.artist_id = ar.id), 0)"#,
        )
        .execute(&self.pool)
        .await
        .map_err(db)?;
        sqlx::query(
            r#"UPDATE podcasts p
               SET storage_bytes = COALESCE(
                   (SELECT SUM(e.file_size) FROM podcast_episodes e WHERE e.podcast_id = p.id), 0)"#,
        )
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn aggregates(&self) -> Result<StorageAggregates> {
        sqlx::query_as::<_, StorageAggregates>(
            r#"SELECT
                 (SELECT COALESCE(SUM(file_size), 0) FROM tracks)            AS music_bytes,
                 (SELECT COALESCE(SUM(file_size), 0) FROM podcast_episodes)  AS podcast_bytes,
                 (SELECT COUNT(*) FROM tracks)                              AS track_count,
                 (SELECT COUNT(*) FROM albums)                              AS album_count,
                 (SELECT COUNT(*) FROM artists)                             AS artist_count,
                 (SELECT COUNT(*) FROM podcasts)                            AS podcast_count,
                 (SELECT COUNT(*) FROM podcast_episodes)                    AS episode_count"#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_library_aggregates(&self, a: StorageAggregates) -> Result<()> {
        sqlx::query(
            r#"UPDATE library_storage SET
                 music_bytes = $1, podcast_bytes = $2,
                 track_count = $3, album_count = $4, artist_count = $5,
                 podcast_count = $6, episode_count = $7,
                 total_bytes = $1 + $2 + artwork_bytes + other_bytes,
                 computed_at = now()
               WHERE id = 1"#,
        )
        .bind(a.music_bytes)
        .bind(a.podcast_bytes)
        .bind(a.track_count)
        .bind(a.album_count)
        .bind(a.artist_count)
        .bind(a.podcast_count)
        .bind(a.episode_count)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn set_library_disk(&self, artwork_bytes: i64, other_bytes: i64) -> Result<()> {
        sqlx::query(
            r#"UPDATE library_storage SET
                 artwork_bytes = $1, other_bytes = $2,
                 total_bytes = music_bytes + podcast_bytes + $1 + $2,
                 computed_at = now()
               WHERE id = 1"#,
        )
        .bind(artwork_bytes)
        .bind(other_bytes)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn get_library_storage(&self) -> Result<LibraryStorage> {
        sqlx::query_as::<_, LibraryStorage>(
            r#"SELECT music_bytes, podcast_bytes, artwork_bytes, other_bytes,
                      total_bytes, track_count, album_count, artist_count,
                      podcast_count, episode_count, computed_at
               FROM library_storage WHERE id = 1"#,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }
}
