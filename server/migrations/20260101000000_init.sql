-- Phase 1: core schema.
--
-- Designed to remain portable to the client's SQLite offline cache:
--   * UUIDs (TEXT-compatible on SQLite)
--   * TEXT + CHECK constraints instead of PG enums
--   * JSON stored as TEXT (not JSONB) so the same column maps cleanly to SQLite
--   * No arrays, no tsvector, no PG-only types
--
-- All `*_at` columns are TIMESTAMPTZ on Postgres; the client mirror uses
-- ISO-8601 TEXT.

-- Built-in on PG13+; safety net for older deployments.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ---------------------------------------------------------------------------
-- users
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    username         TEXT        NOT NULL UNIQUE,
    password_hash    TEXT        NOT NULL,
    permission_level TEXT        NOT NULL
                                 CHECK (permission_level IN ('admin', 'manager', 'user')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- artists
-- ---------------------------------------------------------------------------
CREATE TABLE artists (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT        NOT NULL,
    sort_name  TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_artists_name ON artists(name);

-- ---------------------------------------------------------------------------
-- albums
-- ---------------------------------------------------------------------------
CREATE TABLE albums (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    artist_id    UUID        NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    title        TEXT        NOT NULL,
    release_year INTEGER,
    cover_path   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_albums_artist ON albums(artist_id);

-- ---------------------------------------------------------------------------
-- tracks
-- ---------------------------------------------------------------------------
CREATE TABLE tracks (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    album_id      UUID        NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    artist_id     UUID        NOT NULL REFERENCES artists(id) ON DELETE RESTRICT,
    title         TEXT        NOT NULL,
    track_no      INTEGER,
    disc_no       INTEGER,
    duration_ms   BIGINT      NOT NULL,
    codec         TEXT        NOT NULL,
    bitrate_kbps  INTEGER,
    file_path     TEXT        NOT NULL UNIQUE,
    file_size     BIGINT,
    -- JSON-as-TEXT for portability; validated at the app layer.
    metadata_json TEXT        NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_tracks_album  ON tracks(album_id);
CREATE INDEX idx_tracks_artist ON tracks(artist_id);

-- ---------------------------------------------------------------------------
-- playlists
-- ---------------------------------------------------------------------------
CREATE TABLE playlists (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id   UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_playlists_owner ON playlists(owner_id);

CREATE TABLE playlist_tracks (
    playlist_id UUID        NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    UUID        NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
    position    INTEGER     NOT NULL,
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (playlist_id, position)
);
CREATE INDEX idx_playlist_tracks_track ON playlist_tracks(track_id);

-- ---------------------------------------------------------------------------
-- follows (user -> artist)
-- ---------------------------------------------------------------------------
CREATE TABLE follows (
    user_id    UUID        NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    artist_id  UUID        NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, artist_id)
);
CREATE INDEX idx_follows_artist ON follows(artist_id);

-- ---------------------------------------------------------------------------
-- audit_log
-- ---------------------------------------------------------------------------
CREATE TABLE audit_log (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- nullable: SECRET_KEY-authenticated or system-driven actions have no user.
    actor_id    UUID        REFERENCES users(id) ON DELETE SET NULL,
    action      TEXT        NOT NULL,                      -- e.g. 'track.update'
    entity_type TEXT        NOT NULL,                      -- e.g. 'track'
    entity_id   UUID,                                       -- nullable for bulk ops
    before_json TEXT,                                       -- JSON; nullable for creates
    after_json  TEXT,                                       -- JSON; nullable for deletes
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_entity  ON audit_log(entity_type, entity_id);
CREATE INDEX idx_audit_actor   ON audit_log(actor_id);
CREATE INDEX idx_audit_created ON audit_log(created_at);

-- ---------------------------------------------------------------------------
-- sessions (opaque tokens issued on username/password login)
-- ---------------------------------------------------------------------------
CREATE TABLE sessions (
    token       TEXT        PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ
);
CREATE INDEX idx_sessions_user    ON sessions(user_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);
