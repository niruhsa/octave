-- Phase: Podcasts (client offline cache).
--
-- A partial mirror of the server's `20260801000000_podcasts.sql`: subscribed
-- shows (so the list renders offline) + only DOWNLOADED episodes (offline-cache
-- principle — presence of `local_file_path` == available offline, like
-- `tracks`). Same portability rules as 0001_init.sql (TEXT-UUID ids equal to the
-- server's, TEXT+CHECK, JSON-as-TEXT, ISO-8601 timestamps) so the sync engine
-- reconciles by id.

-- Subscribed shows. Minus the server-only refresh bookkeeping (etag/modified).
CREATE TABLE podcasts (
    id            TEXT PRIMARY KEY,
    feed_url      TEXT NOT NULL,
    title         TEXT NOT NULL,
    author        TEXT,
    description   TEXT,
    image_url     TEXT,                          -- remote art (cover:// proxies it)
    language      TEXT,
    categories    TEXT NOT NULL DEFAULT '[]',    -- JSON array as TEXT
    subscribed    INTEGER NOT NULL DEFAULT 0,    -- 1 = user is subscribed
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_podcasts_title ON podcasts(title);

-- Only DOWNLOADED episodes live here. `local_file_path` present == offline.
CREATE TABLE podcast_episodes (
    id               TEXT PRIMARY KEY,
    podcast_id       TEXT NOT NULL REFERENCES podcasts(id) ON DELETE CASCADE,
    guid             TEXT NOT NULL,
    title            TEXT NOT NULL,
    description      TEXT,
    enclosure_url    TEXT NOT NULL,              -- origin URL (stream when not downloaded)
    episode_no       INTEGER,
    season_no        INTEGER,
    duration_ms      INTEGER,
    codec            TEXT,
    bitrate_kbps     INTEGER,
    file_size        INTEGER,
    local_file_path  TEXT UNIQUE,                -- NULL until downloaded
    image_path       TEXT,
    published_at     TEXT,
    metadata_json    TEXT NOT NULL DEFAULT '{}',
    downloaded_at    TEXT,
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_episodes_podcast ON podcast_episodes(podcast_id, published_at DESC);

-- Widen the sync_state entity_type CHECK to allow the two podcast types.
-- SQLite can't ALTER a CHECK in place, so rebuild the table (it has no FKs in
-- either direction, so the rebuild is safe). Rows + the index are preserved.
CREATE TABLE sync_state_new (
    entity_type     TEXT NOT NULL
                     CHECK (entity_type IN (
                         'artist', 'album', 'track', 'playlist', 'album_art',
                         'podcast', 'podcast_episode'
                     )),
    entity_id       TEXT NOT NULL,
    server_version  TEXT,
    server_etag     TEXT,
    last_synced_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (entity_type, entity_id)
);
INSERT INTO sync_state_new (entity_type, entity_id, server_version, server_etag, last_synced_at)
    SELECT entity_type, entity_id, server_version, server_etag, last_synced_at FROM sync_state;
DROP TABLE sync_state;
ALTER TABLE sync_state_new RENAME TO sync_state;
CREATE INDEX idx_sync_state_synced ON sync_state(last_synced_at);
