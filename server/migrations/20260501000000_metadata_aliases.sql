-- Metadata editing + merge: artist/album aliases and the single-release flag.
--
-- Lets managers merge duplicate artists/albums that differ only in spelling
-- (e.g. a Korean name `우기 ((여자)아이들)` vs. the English `YUQI`) into one
-- canonical row while preserving every original spelling here as an alias.
-- The canonical `artists.name` / `albums.title` is kept as the spelling whose
-- language matches the configured PRIMARY_LANGUAGE, so all existing reads keep
-- showing the right name. A track that was its own "single" album can be moved
-- into its parent album and flagged `is_single_release`.
--
-- Portable to the client's SQLite cache: UUID/TEXT/BOOLEAN/CHECK only, JSON
-- never used here, `*_at` is TIMESTAMPTZ on Postgres / ISO-8601 TEXT on SQLite.

-- ---------------------------------------------------------------------------
-- artist_aliases — every known spelling of an artist.
-- `is_primary` marks the spelling currently mirrored into `artists.name`.
-- `language` is the inferred/declared label (e.g. 'English', 'Korean'); NULL
-- means "infer from the script at read time".
-- ---------------------------------------------------------------------------
CREATE TABLE artist_aliases (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    artist_id  UUID        NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    name       TEXT        NOT NULL,
    sort_name  TEXT,
    language   TEXT,
    is_primary BOOLEAN     NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_artist_aliases_artist ON artist_aliases(artist_id);
CREATE UNIQUE INDEX uq_artist_aliases_name ON artist_aliases(artist_id, name);

-- ---------------------------------------------------------------------------
-- album_aliases — every known spelling of an album title.
-- ---------------------------------------------------------------------------
CREATE TABLE album_aliases (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    album_id   UUID        NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    title      TEXT        NOT NULL,
    language   TEXT,
    is_primary BOOLEAN     NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_album_aliases_album ON album_aliases(album_id);
CREATE UNIQUE INDEX uq_album_aliases_title ON album_aliases(album_id, title);

-- ---------------------------------------------------------------------------
-- tracks.is_single_release — a track that is a "single release" within its
-- album (e.g. moved in from a one-track single album).
-- ---------------------------------------------------------------------------
ALTER TABLE tracks ADD COLUMN is_single_release BOOLEAN NOT NULL DEFAULT false;

-- ---------------------------------------------------------------------------
-- Backfill: seed one primary alias per existing artist/album from its current
-- name/title. `language` stays NULL (resolution infers it from the script when
-- needed), so this stays pure SQL.
-- ---------------------------------------------------------------------------
INSERT INTO artist_aliases (artist_id, name, sort_name, language, is_primary)
SELECT id, name, sort_name, NULL, true FROM artists;

INSERT INTO album_aliases (album_id, title, language, is_primary)
SELECT id, title, NULL, true FROM albums;
