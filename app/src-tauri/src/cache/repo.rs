//! Cache repository — typed CRUD over the SQLite pool.
//!
//! Conventions:
//!   * Every write is an UPSERT keyed by the server-issued primary key.
//!     Sync (Phase 5) repeatedly applies the same row when the server says
//!     it changed; idempotency is mandatory.
//!   * No transactions span multiple rows here — callers wrap multi-row
//!     work in `pool.begin()` when they need atomicity.
//!   * Functions return `AppResult` so handlers can propagate errors
//!     straight across the Tauri bridge.

use sqlx::SqlitePool;

use crate::cache::model::{
    Album, AlbumArt, Artist, PendingOp, Playlist, PlaylistTrack, SyncState, Track,
};
use crate::error::AppResult;

// ---------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------

/// Upsert an artist row. Existing rows are overwritten with the new values
/// and `updated_at` is bumped.
pub async fn upsert_artist(pool: &SqlitePool, artist: &Artist) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO artists (id, name, sort_name, updated_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(id) DO UPDATE SET
            name       = excluded.name,
            sort_name  = excluded.sort_name,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&artist.id)
    .bind(&artist.name)
    .bind(&artist.sort_name)
    .bind(&artist.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_artist(pool: &SqlitePool, id: &str) -> AppResult<Option<Artist>> {
    let row = sqlx::query_as::<_, Artist>("SELECT id, name, sort_name, updated_at FROM artists WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn list_artists(pool: &SqlitePool) -> AppResult<Vec<Artist>> {
    let rows = sqlx::query_as::<_, Artist>(
        "SELECT id, name, sort_name, updated_at FROM artists ORDER BY name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_artist(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM artists WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------

pub async fn upsert_album(pool: &SqlitePool, album: &Album) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO albums (id, artist_id, title, release_year, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(id) DO UPDATE SET
            artist_id    = excluded.artist_id,
            title        = excluded.title,
            release_year = excluded.release_year,
            updated_at   = excluded.updated_at
        "#,
    )
    .bind(&album.id)
    .bind(&album.artist_id)
    .bind(&album.title)
    .bind(album.release_year)
    .bind(&album.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_album(pool: &SqlitePool, id: &str) -> AppResult<Option<Album>> {
    let row = sqlx::query_as::<_, Album>(
        "SELECT id, artist_id, title, release_year, updated_at FROM albums WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn list_albums_by_artist(pool: &SqlitePool, artist_id: &str) -> AppResult<Vec<Album>> {
    let rows = sqlx::query_as::<_, Album>(
        "SELECT id, artist_id, title, release_year, updated_at
         FROM albums WHERE artist_id = ?1
         ORDER BY release_year, title COLLATE NOCASE",
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_album(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM albums WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// album_art
// ---------------------------------------------------------------------------

pub async fn upsert_album_art(pool: &SqlitePool, art: &AlbumArt) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO album_art (album_id, local_cover_path, fetched_at)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(album_id) DO UPDATE SET
            local_cover_path = excluded.local_cover_path,
            fetched_at       = excluded.fetched_at
        "#,
    )
    .bind(&art.album_id)
    .bind(&art.local_cover_path)
    .bind(&art.fetched_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_album_art(pool: &SqlitePool, album_id: &str) -> AppResult<Option<AlbumArt>> {
    let row = sqlx::query_as::<_, AlbumArt>(
        "SELECT album_id, local_cover_path, fetched_at FROM album_art WHERE album_id = ?1",
    )
    .bind(album_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn delete_album_art(pool: &SqlitePool, album_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM album_art WHERE album_id = ?1")
        .bind(album_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// tracks
// ---------------------------------------------------------------------------

pub async fn upsert_track(pool: &SqlitePool, track: &Track) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO tracks (
            id, album_id, artist_id, title, track_no, disc_no,
            duration_ms, codec, bitrate_kbps, file_size,
            local_file_path, metadata_json, downloaded_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        ON CONFLICT(id) DO UPDATE SET
            album_id        = excluded.album_id,
            artist_id       = excluded.artist_id,
            title           = excluded.title,
            track_no        = excluded.track_no,
            disc_no         = excluded.disc_no,
            duration_ms     = excluded.duration_ms,
            codec           = excluded.codec,
            bitrate_kbps    = excluded.bitrate_kbps,
            file_size       = excluded.file_size,
            local_file_path = excluded.local_file_path,
            metadata_json   = excluded.metadata_json,
            updated_at      = excluded.updated_at
        "#,
    )
    .bind(&track.id)
    .bind(&track.album_id)
    .bind(&track.artist_id)
    .bind(&track.title)
    .bind(track.track_no)
    .bind(track.disc_no)
    .bind(track.duration_ms)
    .bind(&track.codec)
    .bind(track.bitrate_kbps)
    .bind(track.file_size)
    .bind(&track.local_file_path)
    .bind(&track.metadata_json)
    .bind(&track.downloaded_at)
    .bind(&track.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_track(pool: &SqlitePool, id: &str) -> AppResult<Option<Track>> {
    let row = sqlx::query_as::<_, Track>(
        r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                 duration_ms, codec, bitrate_kbps, file_size,
                 local_file_path, metadata_json, downloaded_at, updated_at
           FROM tracks WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn list_tracks_by_album(pool: &SqlitePool, album_id: &str) -> AppResult<Vec<Track>> {
    let rows = sqlx::query_as::<_, Track>(
        r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                 duration_ms, codec, bitrate_kbps, file_size,
                 local_file_path, metadata_json, downloaded_at, updated_at
           FROM tracks WHERE album_id = ?1
           ORDER BY disc_no, track_no, title COLLATE NOCASE"#,
    )
    .bind(album_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_downloaded_tracks(pool: &SqlitePool) -> AppResult<Vec<Track>> {
    let rows = sqlx::query_as::<_, Track>(
        r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                 duration_ms, codec, bitrate_kbps, file_size,
                 local_file_path, metadata_json, downloaded_at, updated_at
           FROM tracks
           ORDER BY downloaded_at DESC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_track(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM tracks WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// playlists
// ---------------------------------------------------------------------------

pub async fn upsert_playlist(pool: &SqlitePool, playlist: &Playlist) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO playlists (id, owner_id, name, updated_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(id) DO UPDATE SET
            owner_id   = excluded.owner_id,
            name       = excluded.name,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(&playlist.id)
    .bind(&playlist.owner_id)
    .bind(&playlist.name)
    .bind(&playlist.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_playlists(pool: &SqlitePool) -> AppResult<Vec<Playlist>> {
    let rows = sqlx::query_as::<_, Playlist>(
        "SELECT id, owner_id, name, updated_at FROM playlists ORDER BY name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_playlist(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM playlists WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace a playlist's entire track list in one transaction. Cheaper and
/// simpler than diffing for the offline cache, which is rebuilt from server
/// state on sync.
pub async fn replace_playlist_tracks(
    pool: &SqlitePool,
    playlist_id: &str,
    entries: &[PlaylistTrack],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ?1")
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;
    for entry in entries {
        sqlx::query(
            r#"INSERT INTO playlist_tracks (playlist_id, track_id, position, added_at)
               VALUES (?1, ?2, ?3, ?4)"#,
        )
        .bind(&entry.playlist_id)
        .bind(&entry.track_id)
        .bind(entry.position)
        .bind(&entry.added_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list_playlist_tracks(
    pool: &SqlitePool,
    playlist_id: &str,
) -> AppResult<Vec<PlaylistTrack>> {
    let rows = sqlx::query_as::<_, PlaylistTrack>(
        "SELECT playlist_id, track_id, position, added_at
         FROM playlist_tracks WHERE playlist_id = ?1
         ORDER BY position",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// sync_state
// ---------------------------------------------------------------------------

pub async fn upsert_sync_state(pool: &SqlitePool, state: &SyncState) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO sync_state (entity_type, entity_id, server_version, server_etag, last_synced_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(entity_type, entity_id) DO UPDATE SET
            server_version = excluded.server_version,
            server_etag    = excluded.server_etag,
            last_synced_at = excluded.last_synced_at
        "#,
    )
    .bind(&state.entity_type)
    .bind(&state.entity_id)
    .bind(&state.server_version)
    .bind(&state.server_etag)
    .bind(&state.last_synced_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_sync_state(
    pool: &SqlitePool,
    entity_type: &str,
    entity_id: &str,
) -> AppResult<Option<SyncState>> {
    let row = sqlx::query_as::<_, SyncState>(
        "SELECT entity_type, entity_id, server_version, server_etag, last_synced_at
         FROM sync_state WHERE entity_type = ?1 AND entity_id = ?2",
    )
    .bind(entity_type)
    .bind(entity_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn delete_sync_state(
    pool: &SqlitePool,
    entity_type: &str,
    entity_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM sync_state WHERE entity_type = ?1 AND entity_id = ?2")
        .bind(entity_type)
        .bind(entity_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// pending_ops (offline-edit outbox)
// ---------------------------------------------------------------------------

/// Append an op to the outbox. Returns the new row id.
pub async fn enqueue_op(pool: &SqlitePool, op_type: &str, payload_json: &str) -> AppResult<i64> {
    let row = sqlx::query(
        "INSERT INTO pending_ops (op_type, payload_json) VALUES (?1, ?2)",
    )
    .bind(op_type)
    .bind(payload_json)
    .execute(pool)
    .await?;
    Ok(row.last_insert_rowid())
}

/// All queued ops in insertion order (FIFO replay).
pub async fn list_pending_ops(pool: &SqlitePool) -> AppResult<Vec<PendingOp>> {
    let rows = sqlx::query_as::<_, PendingOp>(
        "SELECT id, op_type, payload_json, created_at, attempts, last_error
         FROM pending_ops ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn count_pending_ops(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_ops")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn delete_pending_op(pool: &SqlitePool, id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM pending_ops WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Bump attempt count + record the last error for an op that failed but
/// should stay queued (transport-level failure).
pub async fn mark_op_failed(pool: &SqlitePool, id: i64, error: &str) -> AppResult<()> {
    sqlx::query("UPDATE pending_ops SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1")
        .bind(id)
        .bind(error)
        .execute(pool)
        .await?;
    Ok(())
}
