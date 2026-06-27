-- Phase: Library storage stats + per-song media info.
--
-- Two related additions:
--   1. Per-track audio-quality detail (sample rate / bit depth / channels) so
--      the client can surface a media-info panel (e.g. "24/96", "Lossless").
--      Captured at ingest/rescan from the same lofty probe as codec/bitrate.
--   2. Denormalized storage accounting: a `storage_bytes` rollup on each
--      catalog entity (artist/album/podcast) plus a singleton `library_storage`
--      row holding the global breakdown (music / podcast / artwork / other).
--      Recomputed on scan + upload and refreshed by a 24h background job so the
--      homepage widget is a single fast read.
--
-- Same portability rules as every prior migration (UUID/TEXT/BIGINT/INTEGER/
-- CHECK/TIMESTAMPTZ) so the client's SQLite cache mirrors it 1:1.

-- ---------------------------------------------------------------------------
-- tracks: audio-quality detail. Nullable — unknown until probed, and some
-- formats/probes don't report bit depth.
-- ---------------------------------------------------------------------------
ALTER TABLE tracks
    ADD COLUMN sample_rate_hz INTEGER,
    ADD COLUMN bit_depth      INTEGER,
    ADD COLUMN channels       INTEGER;

-- ---------------------------------------------------------------------------
-- Per-entity storage rollups. SUM of the owned files' on-disk bytes, kept up
-- to date by StorageService::recompute_aggregates (cheap SQL) on every scan /
-- upload. Default 0 so existing rows read sensibly until the first recompute.
-- ---------------------------------------------------------------------------
ALTER TABLE artists  ADD COLUMN storage_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE albums   ADD COLUMN storage_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE podcasts ADD COLUMN storage_bytes BIGINT NOT NULL DEFAULT 0;

-- ---------------------------------------------------------------------------
-- library_storage: a single row (id = 1) holding the global breakdown. The UI
-- shows `misc = artwork_bytes + other_bytes`. `music_bytes`/`podcast_bytes` are
-- SQL sums of the respective file_size columns; `artwork_bytes`/`other_bytes`
-- come from a filesystem walk (artwork dir vs. non-audio files elsewhere).
-- ---------------------------------------------------------------------------
CREATE TABLE library_storage (
    id             INTEGER     PRIMARY KEY CHECK (id = 1),
    music_bytes    BIGINT      NOT NULL DEFAULT 0,
    podcast_bytes  BIGINT      NOT NULL DEFAULT 0,
    artwork_bytes  BIGINT      NOT NULL DEFAULT 0,
    other_bytes    BIGINT      NOT NULL DEFAULT 0,
    total_bytes    BIGINT      NOT NULL DEFAULT 0,
    track_count    BIGINT      NOT NULL DEFAULT 0,
    album_count    BIGINT      NOT NULL DEFAULT 0,
    artist_count   BIGINT      NOT NULL DEFAULT 0,
    podcast_count  BIGINT      NOT NULL DEFAULT 0,
    episode_count  BIGINT      NOT NULL DEFAULT 0,
    computed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Seed the singleton so a read before the first recompute returns zeros
-- instead of an empty result the service has to special-case.
INSERT INTO library_storage (id) VALUES (1);
