-- Phase 1: client-side offline cache.
--
-- This database is a *partial* mirror of the server's PostgreSQL schema —
-- it stores ONLY items the user has downloaded for offline use. Never the
-- full catalog. The server (PostgreSQL) is authority when online.
--
-- Portability rules mirrored from the server migration:
--   * UUIDs stored as TEXT (server uses UUID, identical at the wire layer).
--   * TEXT + CHECK instead of enums.
--   * JSON stored as TEXT.
--   * ISO-8601 TEXT timestamps to match the server's TIMESTAMPTZ values
--     when serialised.
--
-- Reconciliation contract: every `id` in this DB equals the server's `id`
-- byte-for-byte. Phase 5 (Sync Engine) relies on this.

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- artists  (metadata for downloaded content only)
-- ---------------------------------------------------------------------------
CREATE TABLE artists (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    sort_name   TEXT,
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_artists_name ON artists(name);

-- ---------------------------------------------------------------------------
-- albums  (metadata for downloaded content only)
-- ---------------------------------------------------------------------------
CREATE TABLE albums (
    id            TEXT PRIMARY KEY,
    artist_id     TEXT NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    release_year  INTEGER,
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_albums_artist ON albums(artist_id);

-- ---------------------------------------------------------------------------
-- album_art  (local cover path per album)
-- One row per album that has art downloaded locally. Distinct from the
-- albums table so we can track per-cover fetch state independently.
-- ---------------------------------------------------------------------------
CREATE TABLE album_art (
    album_id          TEXT PRIMARY KEY REFERENCES albums(id) ON DELETE CASCADE,
    local_cover_path  TEXT NOT NULL,
    fetched_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- ---------------------------------------------------------------------------
-- tracks  (only fully-downloaded tracks live here)
--
-- `local_file_path` is the on-disk path inside the app's data dir. Presence
-- of a row in this table is the source of truth for "this track is
-- available offline".
-- ---------------------------------------------------------------------------
CREATE TABLE tracks (
    id               TEXT PRIMARY KEY,
    album_id         TEXT NOT NULL REFERENCES albums(id)  ON DELETE CASCADE,
    artist_id        TEXT NOT NULL REFERENCES artists(id) ON DELETE RESTRICT,
    title            TEXT NOT NULL,
    track_no         INTEGER,
    disc_no          INTEGER,
    duration_ms      INTEGER NOT NULL,
    codec            TEXT NOT NULL,
    bitrate_kbps     INTEGER,
    file_size        INTEGER,
    local_file_path  TEXT NOT NULL UNIQUE,
    metadata_json    TEXT NOT NULL DEFAULT '{}',
    downloaded_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_tracks_album  ON tracks(album_id);
CREATE INDEX idx_tracks_artist ON tracks(artist_id);

-- ---------------------------------------------------------------------------
-- playlists  (server-side IDs; cached for offline view + edit-queue)
-- ---------------------------------------------------------------------------
CREATE TABLE playlists (
    id          TEXT PRIMARY KEY,
    owner_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_playlists_owner ON playlists(owner_id);

-- playlist_tracks references tracks loosely — a playlist may include tracks
-- that aren't downloaded yet. The UI marks those as "stream-only / not
-- offline" when the server is unreachable.
CREATE TABLE playlist_tracks (
    playlist_id  TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id     TEXT NOT NULL,
    position     INTEGER NOT NULL,
    added_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (playlist_id, position)
);
CREATE INDEX idx_playlist_tracks_track ON playlist_tracks(track_id);

-- ---------------------------------------------------------------------------
-- sync_state  (per-entity reconciliation bookkeeping)
--
-- Keyed by (entity_type, entity_id) so Phase 5 can diff server etags/versions
-- against the last value we observed for each cached row. Rows are created
-- lazily when we first sync an entity.
-- ---------------------------------------------------------------------------
CREATE TABLE sync_state (
    entity_type     TEXT NOT NULL
                     CHECK (entity_type IN ('artist', 'album', 'track', 'playlist', 'album_art')),
    entity_id       TEXT NOT NULL,
    server_version  TEXT,
    server_etag     TEXT,
    last_synced_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (entity_type, entity_id)
);
CREATE INDEX idx_sync_state_synced ON sync_state(last_synced_at);
