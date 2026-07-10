-- Phase 15: per-track lyrics (synced + plain).
--
-- The lyric text lives on disk as an `.lrc` under LYRICS_PATH (like artwork
-- under ARTWORK_PATH); the row only carries the pointer + provenance so the
-- "needs lyrics?" scan and the client cache stay cheap. `lyrics_synced`
-- distinguishes a time-aligned `.lrc` from a plain-text dump.
-- `lyrics_instrumental` records a positive "this track has no lyrics" result so
-- the pass doesn't refetch it forever. `lyrics_source_sig` is the file-content
-- signature (size+mtime) at resolution time, so a replaced audio file
-- re-resolves — mirrors the artwork / fingerprint freshness check.
--
-- Append-only: this is a new file over the immutable prior migrations; the
-- columns are nullable / defaulted so every existing `tracks` read is
-- unaffected, and the values are portable to the client's SQLite cache (only
-- BOOLEAN + TEXT + a timestamp).
ALTER TABLE tracks
    ADD COLUMN lyrics_path         TEXT,
    ADD COLUMN lyrics_synced       BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN lyrics_source       TEXT,
    ADD COLUMN lyrics_instrumental BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN lyrics_source_sig   TEXT,
    ADD COLUMN lyrics_synced_at    TIMESTAMPTZ;

-- Drives the "which tracks still need a lyric lookup" background scan.
CREATE INDEX idx_tracks_lyrics_pending
    ON tracks (id)
    WHERE lyrics_path IS NULL AND NOT lyrics_instrumental;
