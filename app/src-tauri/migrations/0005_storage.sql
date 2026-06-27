-- Phase: Library storage stats + per-song media info (client offline cache).
--
-- Mirrors the server's `20260901000000_storage.sql` so the per-entity storage
-- rollups and the per-track audio-quality detail are available offline for
-- cached/downloaded items. The global `library_storage` breakdown is NOT
-- mirrored — the homepage widget is an online read of the server's live view.
--
-- Same portability rules as the earlier migrations (TEXT-UUID ids equal to the
-- server's, nullable INTEGERs) so the sync engine reconciles by id.

-- Per-track audio-quality detail (sample rate Hz / bit depth / channels).
ALTER TABLE tracks ADD COLUMN sample_rate_hz INTEGER;
ALTER TABLE tracks ADD COLUMN bit_depth      INTEGER;
ALTER TABLE tracks ADD COLUMN channels       INTEGER;

-- Per-entity storage rollups (server-computed SUM of owned files' bytes).
ALTER TABLE artists  ADD COLUMN storage_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE albums   ADD COLUMN storage_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE podcasts ADD COLUMN storage_bytes INTEGER NOT NULL DEFAULT 0;
