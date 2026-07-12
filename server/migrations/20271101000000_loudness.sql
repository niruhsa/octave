-- Phase 16: loudness normalization (ReplayGain / EBU R128).
--
-- Each track's integrated loudness (LUFS, ITU-R BS.1770 / EBU R128) and sample
-- peak (linear amplitude) are measured on the server during the fingerprint
-- PCM-decode pass (services/fingerprint/) — the same decode that produces the
-- similarity embedding, so loudness is a free add-on rather than a second
-- full-file decode. The client applies a compensating gain in the player so
-- every track plays at a consistent perceived loudness.
--
-- `album_loudness_lufs` is denormalized onto each track (from the album rollup
-- below) so the player gets album-mode gain with no JOIN — the queue is
-- track-centric and rarely has the album row loaded at playback time.
-- `loudness_source_sig` is the file-content signature (size+mtime) at
-- measurement time so a re-encoded/replaced file is re-measured, mirroring the
-- lyrics / fingerprint freshness check.
--
-- Append-only over the immutable prior migrations; every column is
-- nullable/defaulted so existing `tracks`/`albums` reads are unaffected, and the
-- values are portable to the client's SQLite cache (REAL + TEXT + a timestamp).
ALTER TABLE tracks
    ADD COLUMN loudness_lufs        REAL,
    ADD COLUMN loudness_peak        REAL,
    ADD COLUMN album_loudness_lufs  REAL,
    ADD COLUMN loudness_source_sig  TEXT,
    ADD COLUMN loudness_analyzed_at TIMESTAMPTZ;

ALTER TABLE albums
    ADD COLUMN loudness_lufs REAL,
    ADD COLUMN loudness_peak REAL;

-- Drives the "which tracks still need a loudness measurement" background scan.
CREATE INDEX idx_tracks_loudness_pending
    ON tracks (id)
    WHERE loudness_source_sig IS NULL;
