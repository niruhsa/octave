-- Track title aliases — alternative spellings of a track title per language.
--
-- Mirrors artist_aliases / album_aliases (20260501000000_metadata_aliases.sql):
-- every known spelling of a track title, with its inferred/declared `language`
-- and an `is_primary` flag marking the spelling currently mirrored into
-- `tracks.title`. The canonical title follows the configured PRIMARY_LANGUAGE
-- (resolved in LibraryService), so all existing reads keep showing the right
-- title. `language` NULL means "infer from the script at read time".
--
-- Portable to the client's SQLite cache: UUID/TEXT/BOOLEAN only, JSON never
-- used here, `*_at` is TIMESTAMPTZ on Postgres / ISO-8601 TEXT on SQLite.

CREATE TABLE track_aliases (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    track_id   UUID        NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    title      TEXT        NOT NULL,
    language   TEXT,
    is_primary BOOLEAN     NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_track_aliases_track ON track_aliases(track_id);
CREATE UNIQUE INDEX uq_track_aliases_title ON track_aliases(track_id, title);

-- Backfill: seed one primary alias per existing track from its current title.
-- `language` stays NULL (inferred at read time), so this stays pure SQL.
INSERT INTO track_aliases (track_id, title, language, is_primary)
SELECT id, title, NULL, true FROM tracks;
