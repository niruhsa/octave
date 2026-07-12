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
    Album, AlbumArt, Artist, PendingOp, PendingPlay, Playlist, PlaylistTrack, Podcast,
    PodcastEpisode, PodcastEpisodeProgress, SyncState, Track, TrackLyricsRow,
};
use crate::error::AppResult;

// ---------------------------------------------------------------------------
// Lyrics (Phase 15) — offline mirror of a track's parsed lyrics.
// ---------------------------------------------------------------------------

/// Persist (or refresh) a track's parsed lyrics for offline reads. Idempotent.
pub async fn upsert_track_lyrics(
    pool: &SqlitePool,
    track_id: &str,
    lyrics: &crate::transport::Lyrics,
) -> AppResult<()> {
    let lines_json = serde_json::to_string(&lyrics.lines).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        r#"
        INSERT INTO track_lyrics (track_id, found, synced, instrumental, source, lines_json, plain)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(track_id) DO UPDATE SET
            found        = excluded.found,
            synced       = excluded.synced,
            instrumental = excluded.instrumental,
            source       = excluded.source,
            lines_json   = excluded.lines_json,
            plain        = excluded.plain,
            fetched_at   = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        "#,
    )
    .bind(track_id)
    .bind(lyrics.found as i64)
    .bind(lyrics.synced as i64)
    .bind(lyrics.instrumental as i64)
    .bind(&lyrics.source)
    .bind(lines_json)
    .bind(&lyrics.plain)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read a track's cached lyrics (offline fallback). `None` when never cached.
pub async fn get_track_lyrics(
    pool: &SqlitePool,
    track_id: &str,
) -> AppResult<Option<crate::transport::Lyrics>> {
    let row = sqlx::query_as::<_, TrackLyricsRow>(
        r#"SELECT track_id, found, synced, instrumental, source, lines_json, plain
           FROM track_lyrics WHERE track_id = ?1"#,
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(track_lyrics_from_row))
}

fn track_lyrics_from_row(r: TrackLyricsRow) -> crate::transport::Lyrics {
    let lines =
        serde_json::from_str::<Vec<crate::transport::LyricLine>>(&r.lines_json).unwrap_or_default();
    crate::transport::Lyrics {
        found: r.found != 0,
        synced: r.synced != 0,
        instrumental: r.instrumental != 0,
        source: r.source,
        lines,
        plain: r.plain,
    }
}

// ---------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------

/// Upsert an artist row. Existing rows are overwritten with the new values
/// and `updated_at` is bumped.
pub async fn upsert_artist(pool: &SqlitePool, artist: &Artist) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO artists (id, name, sort_name, storage_bytes, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(id) DO UPDATE SET
            name          = excluded.name,
            sort_name     = excluded.sort_name,
            storage_bytes = excluded.storage_bytes,
            updated_at    = excluded.updated_at
        "#,
    )
    .bind(&artist.id)
    .bind(&artist.name)
    .bind(&artist.sort_name)
    .bind(artist.storage_bytes)
    .bind(&artist.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_artist(pool: &SqlitePool, id: &str) -> AppResult<Option<Artist>> {
    let row = sqlx::query_as::<_, Artist>("SELECT id, name, sort_name, storage_bytes, updated_at FROM artists WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn list_artists(pool: &SqlitePool) -> AppResult<Vec<Artist>> {
    let rows = sqlx::query_as::<_, Artist>(
        "SELECT id, name, sort_name, storage_bytes, updated_at FROM artists ORDER BY name COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_artist(pool: &SqlitePool, id: &str) -> AppResult<()> {
    // Cascade manually: tracks.artist_id has ON DELETE RESTRICT, so a plain
    // DELETE FROM artists would fail (error 1811) if any tracks remain.
    // Must remove tracks → album_art → albums → artist, in that order.
    sqlx::query("DELETE FROM tracks WHERE artist_id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    // Also clean up album art and albums for this artist.
    sqlx::query(
        "DELETE FROM album_art WHERE album_id IN (SELECT id FROM albums WHERE artist_id = ?1)",
    )
    .bind(id)
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM albums WHERE artist_id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
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
        INSERT INTO albums (id, artist_id, title, release_year, storage_bytes, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(id) DO UPDATE SET
            artist_id     = excluded.artist_id,
            title         = excluded.title,
            release_year  = excluded.release_year,
            storage_bytes = excluded.storage_bytes,
            updated_at    = excluded.updated_at
        "#,
    )
    .bind(&album.id)
    .bind(&album.artist_id)
    .bind(&album.title)
    .bind(album.release_year)
    .bind(album.storage_bytes)
    .bind(&album.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_album(pool: &SqlitePool, id: &str) -> AppResult<Option<Album>> {
    let row = sqlx::query_as::<_, Album>(
        "SELECT id, artist_id, title, release_year, storage_bytes, updated_at FROM albums WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn list_albums_by_artist(pool: &SqlitePool, artist_id: &str) -> AppResult<Vec<Album>> {
    let rows = sqlx::query_as::<_, Album>(
        "SELECT id, artist_id, title, release_year, storage_bytes, updated_at
         FROM albums WHERE artist_id = ?1
         ORDER BY release_year, title COLLATE NOCASE",
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn delete_album(pool: &SqlitePool, id: &str) -> AppResult<()> {
    // Cascade manually: tracks.album_id has ON DELETE CASCADE so a plain
    // delete would work, but being explicit avoids any edge-case FK ordering
    // surprises.
    sqlx::query("DELETE FROM tracks WHERE album_id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM album_art WHERE album_id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
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
            sample_rate_hz, bit_depth, channels,
            loudness_lufs, loudness_peak, album_loudness_lufs,
            local_file_path, metadata_json, downloaded_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
        ON CONFLICT(id) DO UPDATE SET
            album_id            = excluded.album_id,
            artist_id           = excluded.artist_id,
            title               = excluded.title,
            track_no            = excluded.track_no,
            disc_no             = excluded.disc_no,
            duration_ms         = excluded.duration_ms,
            codec               = excluded.codec,
            bitrate_kbps        = excluded.bitrate_kbps,
            file_size           = excluded.file_size,
            sample_rate_hz      = excluded.sample_rate_hz,
            bit_depth           = excluded.bit_depth,
            channels            = excluded.channels,
            loudness_lufs       = excluded.loudness_lufs,
            loudness_peak       = excluded.loudness_peak,
            album_loudness_lufs = excluded.album_loudness_lufs,
            local_file_path     = excluded.local_file_path,
            metadata_json       = excluded.metadata_json,
            updated_at          = excluded.updated_at
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
    .bind(track.sample_rate_hz)
    .bind(track.bit_depth)
    .bind(track.channels)
    .bind(track.loudness_lufs)
    .bind(track.loudness_peak)
    .bind(track.album_loudness_lufs)
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
                 sample_rate_hz, bit_depth, channels,
                 loudness_lufs, loudness_peak, album_loudness_lufs,
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
                 sample_rate_hz, bit_depth, channels,
                 loudness_lufs, loudness_peak, album_loudness_lufs,
                 local_file_path, metadata_json, downloaded_at, updated_at
           FROM tracks WHERE album_id = ?1
           ORDER BY disc_no, track_no, title COLLATE NOCASE"#,
    )
    .bind(album_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_tracks_by_artist(pool: &SqlitePool, artist_id: &str) -> AppResult<Vec<Track>> {
    let rows = sqlx::query_as::<_, Track>(
        r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                 duration_ms, codec, bitrate_kbps, file_size,
                 sample_rate_hz, bit_depth, channels,
                 loudness_lufs, loudness_peak, album_loudness_lufs,
                 local_file_path, metadata_json, downloaded_at, updated_at
           FROM tracks WHERE artist_id = ?1
           ORDER BY album_id, disc_no, track_no, title COLLATE NOCASE"#,
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn list_downloaded_tracks(pool: &SqlitePool) -> AppResult<Vec<Track>> {
    let rows = sqlx::query_as::<_, Track>(
        r#"SELECT id, album_id, artist_id, title, track_no, disc_no,
                 duration_ms, codec, bitrate_kbps, file_size,
                 sample_rate_hz, bit_depth, channels,
                 loudness_lufs, loudness_peak, album_loudness_lufs,
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

// ---------------------------------------------------------------------------
// pending_plays (Phase 11 — play-history send-only outbox)
// ---------------------------------------------------------------------------

/// Queue a play for flush. `played_at` defaults (in SQL) to the insert time,
/// which is when the play happened.
pub async fn enqueue_play(
    pool: &SqlitePool,
    id: &str,
    track_id: &str,
    ms_played: i64,
    completed: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO pending_plays (id, track_id, ms_played, completed) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(id)
    .bind(track_id)
    .bind(ms_played)
    .bind(if completed { 1 } else { 0 })
    .execute(pool)
    .await?;
    Ok(())
}

/// Oldest-first batch of queued plays (FIFO flush), capped at `limit`.
pub async fn list_pending_plays(pool: &SqlitePool, limit: i64) -> AppResult<Vec<PendingPlay>> {
    let rows = sqlx::query_as::<_, PendingPlay>(
        "SELECT id, track_id, ms_played, completed, played_at
         FROM pending_plays ORDER BY created_at, id LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Delete the flushed plays by id (called after a successful server push).
pub async fn delete_pending_plays(pool: &SqlitePool, ids: &[String]) -> AppResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for id in ids {
        sqlx::query("DELETE FROM pending_plays WHERE id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn count_pending_plays(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_plays")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// settings (Phase 6 — download prefs: root override, Wi-Fi-only toggle)
// ---------------------------------------------------------------------------

pub async fn get_setting(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    let row = sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_setting(pool: &SqlitePool, key: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM settings WHERE key = ?1")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// storage accounting (Phase 6)
// ---------------------------------------------------------------------------

/// Total bytes used by downloaded tracks (sum of `file_size` where known).
/// Tracks without a recorded `file_size` are skipped — they still count
/// toward the row count but not the byte total.
pub async fn downloaded_bytes(pool: &SqlitePool) -> AppResult<i64> {
    let total: Option<i64> =
        sqlx::query_scalar("SELECT COALESCE(SUM(file_size), 0) FROM tracks WHERE file_size IS NOT NULL")
            .fetch_one(pool)
            .await?;
    Ok(total.unwrap_or(0))
}

/// Count of downloaded tracks (rows in `tracks`). Used for storage accounting.
pub async fn count_downloaded_tracks(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Count of cached album covers (one row per downloaded cover).
pub async fn downloaded_cover_count(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album_art")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Count of downloaded tracks whose album is `album_id`. Used by the
/// delete flow to decide whether to drop the album's cover row.
pub async fn count_downloaded_tracks_for_album(pool: &SqlitePool, album_id: &str) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks WHERE album_id = ?1")
        .bind(album_id)
        .fetch_one(pool)
        .await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// podcasts
// ---------------------------------------------------------------------------

const PODCAST_COLS: &str = "id, feed_url, title, author, description, image_url, \
     language, categories, subscribed, storage_bytes, updated_at";

/// Upsert a podcast show. `subscribed` is preserved by the caller (the merged
/// service writes the current flag), so a metadata-only sync doesn't clobber it.
pub async fn upsert_podcast(pool: &SqlitePool, p: &Podcast) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO podcasts
            (id, feed_url, title, author, description, image_url, language,
             categories, subscribed, storage_bytes, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(id) DO UPDATE SET
            feed_url      = excluded.feed_url,
            title         = excluded.title,
            author        = excluded.author,
            description   = excluded.description,
            image_url     = excluded.image_url,
            language      = excluded.language,
            categories    = excluded.categories,
            subscribed    = excluded.subscribed,
            storage_bytes = excluded.storage_bytes,
            updated_at    = excluded.updated_at
        "#,
    )
    .bind(&p.id)
    .bind(&p.feed_url)
    .bind(&p.title)
    .bind(&p.author)
    .bind(&p.description)
    .bind(&p.image_url)
    .bind(&p.language)
    .bind(&p.categories)
    .bind(p.subscribed)
    .bind(p.storage_bytes)
    .bind(&p.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_podcast(pool: &SqlitePool, id: &str) -> AppResult<Option<Podcast>> {
    let row = sqlx::query_as::<_, Podcast>(&format!(
        "SELECT {PODCAST_COLS} FROM podcasts WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every cached show (subscribed + shows backing downloaded episodes). Drives
/// the sync reconcile pass.
pub async fn list_all_podcasts(pool: &SqlitePool) -> AppResult<Vec<Podcast>> {
    let rows = sqlx::query_as::<_, Podcast>(&format!(
        "SELECT {PODCAST_COLS} FROM podcasts ORDER BY title COLLATE NOCASE"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Shows the user is subscribed to (for the offline subscription list).
pub async fn list_subscribed_podcasts(pool: &SqlitePool) -> AppResult<Vec<Podcast>> {
    let rows = sqlx::query_as::<_, Podcast>(&format!(
        "SELECT {PODCAST_COLS} FROM podcasts WHERE subscribed = 1 \
         ORDER BY title COLLATE NOCASE"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Flip the `subscribed` flag without touching the rest of the row.
pub async fn set_podcast_subscribed(
    pool: &SqlitePool,
    id: &str,
    subscribed: bool,
) -> AppResult<()> {
    sqlx::query("UPDATE podcasts SET subscribed = ?2 WHERE id = ?1")
        .bind(id)
        .bind(if subscribed { 1 } else { 0 })
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_podcast(pool: &SqlitePool, id: &str) -> AppResult<()> {
    // podcast_episodes cascade via the FK; be explicit to avoid ordering edge cases.
    sqlx::query("DELETE FROM podcast_episodes WHERE podcast_id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM podcasts WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// podcast_episodes — cached episode metadata for subscribed shows + the subset
// that is downloaded for offline use. A row with `local_file_path` set is
// downloaded (the offline-cache source of truth, like `tracks`); a row with
// `local_file_path` NULL is metadata only (so a show's episode list renders
// instantly on reopen without re-paging the whole feed from the server).
// ---------------------------------------------------------------------------

const EPISODE_COLS: &str = "id, podcast_id, guid, title, description, enclosure_url, \
     episode_no, season_no, duration_ms, codec, bitrate_kbps, file_size, \
     local_file_path, image_path, published_at, metadata_json, downloaded_at, updated_at";

pub async fn upsert_episode(pool: &SqlitePool, e: &PodcastEpisode) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO podcast_episodes
            (id, podcast_id, guid, title, description, enclosure_url, episode_no,
             season_no, duration_ms, codec, bitrate_kbps, file_size,
             local_file_path, image_path, published_at, metadata_json,
             downloaded_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(id) DO UPDATE SET
            podcast_id      = excluded.podcast_id,
            guid            = excluded.guid,
            title           = excluded.title,
            description     = excluded.description,
            enclosure_url   = excluded.enclosure_url,
            episode_no      = excluded.episode_no,
            season_no       = excluded.season_no,
            duration_ms     = excluded.duration_ms,
            codec           = excluded.codec,
            bitrate_kbps    = excluded.bitrate_kbps,
            file_size       = excluded.file_size,
            local_file_path = excluded.local_file_path,
            image_path      = excluded.image_path,
            published_at    = excluded.published_at,
            metadata_json   = excluded.metadata_json,
            updated_at      = excluded.updated_at
        "#,
    )
    .bind(&e.id)
    .bind(&e.podcast_id)
    .bind(&e.guid)
    .bind(&e.title)
    .bind(&e.description)
    .bind(&e.enclosure_url)
    .bind(e.episode_no)
    .bind(e.season_no)
    .bind(e.duration_ms)
    .bind(&e.codec)
    .bind(e.bitrate_kbps)
    .bind(e.file_size)
    .bind(&e.local_file_path)
    .bind(&e.image_path)
    .bind(&e.published_at)
    .bind(&e.metadata_json)
    .bind(&e.downloaded_at)
    .bind(&e.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_episode(pool: &SqlitePool, id: &str) -> AppResult<Option<PodcastEpisode>> {
    let row = sqlx::query_as::<_, PodcastEpisode>(&format!(
        "SELECT {EPISODE_COLS} FROM podcast_episodes WHERE id = ?1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// All cached episodes for one show (metadata + downloaded), newest-first.
pub async fn list_episodes_for_podcast(
    pool: &SqlitePool,
    podcast_id: &str,
) -> AppResult<Vec<PodcastEpisode>> {
    let rows = sqlx::query_as::<_, PodcastEpisode>(&format!(
        "SELECT {EPISODE_COLS} FROM podcast_episodes WHERE podcast_id = ?1 \
         ORDER BY published_at DESC"
    ))
    .bind(podcast_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every guid cached for one show — the snapshot the incremental sync compares
/// each fetched server page against to find where it overlaps the cache.
pub async fn list_episode_guids(pool: &SqlitePool, podcast_id: &str) -> AppResult<Vec<String>> {
    let guids =
        sqlx::query_scalar::<_, String>("SELECT guid FROM podcast_episodes WHERE podcast_id = ?1")
            .bind(podcast_id)
            .fetch_all(pool)
            .await?;
    Ok(guids)
}

/// Every downloaded episode (storage / downloads list). Metadata-only rows
/// (`local_file_path` NULL) are excluded.
pub async fn list_downloaded_episodes(pool: &SqlitePool) -> AppResult<Vec<PodcastEpisode>> {
    let rows = sqlx::query_as::<_, PodcastEpisode>(&format!(
        "SELECT {EPISODE_COLS} FROM podcast_episodes \
         WHERE local_file_path IS NOT NULL ORDER BY downloaded_at DESC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Upsert episode **metadata** (the incremental-sync path). Unlike
/// [`upsert_episode`], this never touches the client-owned download columns
/// (`local_file_path`, `downloaded_at`, `file_size`, `codec`, `bitrate_kbps`,
/// `image_path`) — so syncing a show's episode list can't clobber a row the
/// user has already downloaded. A downloaded episode also keeps its measured
/// `duration_ms` (the feed value is usually coarser).
pub async fn upsert_episode_meta(pool: &SqlitePool, e: &PodcastEpisode) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO podcast_episodes
            (id, podcast_id, guid, title, description, enclosure_url, episode_no,
             season_no, duration_ms, published_at, metadata_json, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET
            podcast_id    = excluded.podcast_id,
            guid          = excluded.guid,
            title         = excluded.title,
            description   = excluded.description,
            enclosure_url = excluded.enclosure_url,
            episode_no    = excluded.episode_no,
            season_no     = excluded.season_no,
            duration_ms   = CASE WHEN podcast_episodes.local_file_path IS NOT NULL
                                 THEN podcast_episodes.duration_ms
                                 ELSE excluded.duration_ms END,
            published_at  = excluded.published_at,
            updated_at    = excluded.updated_at
        "#,
    )
    .bind(&e.id)
    .bind(&e.podcast_id)
    .bind(&e.guid)
    .bind(&e.title)
    .bind(&e.description)
    .bind(&e.enclosure_url)
    .bind(e.episode_no)
    .bind(e.season_no)
    .bind(e.duration_ms)
    .bind(&e.published_at)
    .bind(&e.metadata_json)
    .bind(&e.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Batch [`upsert_episode_meta`] in a single transaction — one `BEGIN`/`COMMIT`
/// for the whole page instead of an autocommit per row, which is what makes the
/// first sync of a large feed fast (and keeps a steady-state sync cheap). Empty
/// input is a no-op (no transaction).
pub async fn upsert_episodes_meta_batch(
    pool: &SqlitePool,
    episodes: &[PodcastEpisode],
) -> AppResult<()> {
    if episodes.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for e in episodes {
        sqlx::query(
            r#"
            INSERT INTO podcast_episodes
                (id, podcast_id, guid, title, description, enclosure_url, episode_no,
                 season_no, duration_ms, published_at, metadata_json, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                podcast_id    = excluded.podcast_id,
                guid          = excluded.guid,
                title         = excluded.title,
                description   = excluded.description,
                enclosure_url = excluded.enclosure_url,
                episode_no    = excluded.episode_no,
                season_no     = excluded.season_no,
                duration_ms   = CASE WHEN podcast_episodes.local_file_path IS NOT NULL
                                     THEN podcast_episodes.duration_ms
                                     ELSE excluded.duration_ms END,
                published_at  = excluded.published_at,
                updated_at    = excluded.updated_at
            "#,
        )
        .bind(&e.id)
        .bind(&e.podcast_id)
        .bind(&e.guid)
        .bind(&e.title)
        .bind(&e.description)
        .bind(&e.enclosure_url)
        .bind(e.episode_no)
        .bind(e.season_no)
        .bind(e.duration_ms)
        .bind(&e.published_at)
        .bind(&e.metadata_json)
        .bind(&e.updated_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete_episode(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM podcast_episodes WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ----- episode playback progress ------------------------------------------

/// Upsert the listener's progress on one episode (last position + completed).
pub async fn upsert_episode_progress(
    pool: &SqlitePool,
    episode_id: &str,
    position_ms: i64,
    completed: bool,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO podcast_episode_progress (episode_id, position_ms, completed, updated_at)
        VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        ON CONFLICT(episode_id) DO UPDATE SET
            position_ms = excluded.position_ms,
            completed   = excluded.completed,
            updated_at  = excluded.updated_at
        "#,
    )
    .bind(episode_id)
    .bind(position_ms.max(0))
    .bind(if completed { 1 } else { 0 })
    .execute(pool)
    .await?;
    Ok(())
}

/// One episode's cached progress, if any.
pub async fn get_episode_progress(
    pool: &SqlitePool,
    episode_id: &str,
) -> AppResult<Option<PodcastEpisodeProgress>> {
    let row = sqlx::query_as::<_, PodcastEpisodeProgress>(
        "SELECT episode_id, position_ms, completed, updated_at \
         FROM podcast_episode_progress WHERE episode_id = ?1",
    )
    .bind(episode_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// All cached progress rows for one show's episodes (joined via the episode
/// table, which carries `podcast_id`). Drives the list's listened/resume markers.
pub async fn list_episode_progress_for_podcast(
    pool: &SqlitePool,
    podcast_id: &str,
) -> AppResult<Vec<PodcastEpisodeProgress>> {
    let rows = sqlx::query_as::<_, PodcastEpisodeProgress>(
        "SELECT pr.episode_id, pr.position_ms, pr.completed, pr.updated_at \
         FROM podcast_episode_progress pr \
         JOIN podcast_episodes e ON e.id = pr.episode_id \
         WHERE e.podcast_id = ?1",
    )
    .bind(podcast_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Drop metadata-only episodes (not downloaded) whose guid isn't in `keep`.
/// The full-replace path when a refreshed feed shares no episode with the
/// cache. Downloaded episodes are preserved (their audio stays playable).
/// Returns the number of rows removed.
pub async fn delete_stale_metadata_episodes(
    pool: &SqlitePool,
    podcast_id: &str,
    keep: &std::collections::HashSet<String>,
) -> AppResult<u64> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, guid FROM podcast_episodes \
         WHERE podcast_id = ?1 AND local_file_path IS NULL",
    )
    .bind(podcast_id)
    .fetch_all(pool)
    .await?;
    let mut removed = 0u64;
    for (id, guid) in rows {
        if !keep.contains(&guid) {
            sqlx::query("DELETE FROM podcast_episodes WHERE id = ?1")
                .bind(&id)
                .execute(pool)
                .await?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Count of downloaded episodes for a show — drives the delete cover/dir prune.
pub async fn count_downloaded_episodes_for_podcast(
    pool: &SqlitePool,
    podcast_id: &str,
) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM podcast_episodes \
         WHERE podcast_id = ?1 AND local_file_path IS NOT NULL",
    )
    .bind(podcast_id)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Total bytes used by downloaded episodes (sum of `file_size` where known).
pub async fn downloaded_episode_bytes(pool: &SqlitePool) -> AppResult<i64> {
    let total: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(file_size), 0) FROM podcast_episodes \
         WHERE local_file_path IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    Ok(total.unwrap_or(0))
}

/// Count of downloaded episodes (storage accounting).
pub async fn count_downloaded_episodes(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM podcast_episodes WHERE local_file_path IS NOT NULL")
            .fetch_one(pool)
            .await?;
    Ok(n)
}
