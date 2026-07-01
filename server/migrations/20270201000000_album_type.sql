-- Album-level classification: Album / EP / Single.
--
-- Complements the per-track `tracks.is_single_release` flag (added in
-- 20260501000000_metadata_aliases.sql). An album is now typed as one of three
-- kinds. A `single` album is expected to hold 1–3 tracks and always has a main
-- single within it, so the service layer enforces that a `single`-type album
-- has at least one track with `is_single_release = true` (the invariant can't
-- be expressed as a row-local CHECK, so it lives in `LibraryService`).
--
-- Backfill: every existing album defaults to `album`; managers reclassify EPs
-- and singles manually (no auto-classification by track count).
--
-- Portable to the client's SQLite cache: TEXT + CHECK only.

ALTER TABLE albums
    ADD COLUMN album_type TEXT NOT NULL DEFAULT 'album'
    CHECK (album_type IN ('album', 'ep', 'single'));
