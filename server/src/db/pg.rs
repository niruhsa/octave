//! Postgres implementations of the repository traits.
//!
//! Queries are runtime-checked (`sqlx::query` / `query_as`) rather than the
//! `query!` macro so the crate builds without a live database. A future pass
//! can switch to compile-time checking via `cargo sqlx prepare` once the dev
//! DB is part of every contributor's workflow.

use async_trait::async_trait;
use sqlx::PgPool;
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
               RETURNING id, name, sort_name, created_at, updated_at"#,
        )
        .bind(&new.name)
        .bind(&new.sort_name)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, created_at, updated_at
               FROM artists WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, created_at, updated_at
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
            r#"SELECT id, name, sort_name, created_at, updated_at
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
               RETURNING id, name, sort_name, created_at, updated_at"#,
        )
        .bind(id)
        .bind(name)
        .bind(sort_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Artist>> {
        sqlx::query_as::<_, Artist>(
            r#"SELECT id, name, sort_name, created_at, updated_at
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
               RETURNING id, artist_id, title, release_year, cover_path,
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
            r#"SELECT id, artist_id, title, release_year, cover_path,
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
            r#"SELECT id, artist_id, title, release_year, cover_path,
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
            r#"SELECT id, artist_id, title, release_year, cover_path,
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
               RETURNING id, artist_id, title, release_year, cover_path,
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
            r#"SELECT id, artist_id, title, release_year, cover_path,
                       created_at, updated_at
               FROM albums WHERE artist_id = $1 AND title = $2 LIMIT 1"#,
        )
        .bind(artist_id)
        .bind(title)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)
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
                  codec, bitrate_kbps, file_path, file_size, metadata_json)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
               RETURNING id, album_id, artist_id, title, track_no, disc_no,
                         duration_ms, codec, bitrate_kbps, file_path, file_size,
                         metadata_json, created_at, updated_at"#,
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
        .bind(&new.metadata_json)
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Track>> {
        sqlx::query_as::<_, Track>(
            r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                       duration_ms, codec, bitrate_kbps, file_path, file_size,
                       metadata_json, created_at, updated_at
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
                       metadata_json, created_at, updated_at
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
                       metadata_json, created_at, updated_at
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
                         metadata_json, created_at, updated_at"#,
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
                       metadata_json, created_at, updated_at
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
