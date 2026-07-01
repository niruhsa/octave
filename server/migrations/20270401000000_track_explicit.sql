-- Explicit flag — per-track, with an album-level rollup.
--
-- `tracks.is_explicit` lets a song be marked explicit without relying on the
-- title text (mirrors `tracks.is_single_release`). `albums.is_explicit` is a
-- denormalized rollup — true when *any* track on the album is explicit —
-- recomputed by `LibraryService` on the paths that change a track's album
-- membership or explicit flag (like `storage_bytes`), so album reads stay a
-- plain column read (no N+1).
--
-- Backfill: auto-flag existing tracks whose title already carries a
-- bracketed/parenthesized explicit marker (e.g. "[Explicit]", "(Explicit)"),
-- then derive each album's flag from its tracks.
--
-- Portable to the client's SQLite cache: BOOLEAN only.

ALTER TABLE tracks ADD COLUMN is_explicit BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE albums ADD COLUMN is_explicit BOOLEAN NOT NULL DEFAULT false;

UPDATE tracks SET is_explicit = true
WHERE title ~* '[\[(]\s*explicit\s*[\])]';

UPDATE albums a SET is_explicit = EXISTS (
    SELECT 1 FROM tracks t WHERE t.album_id = a.id AND t.is_explicit
);
