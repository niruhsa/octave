-- Phase 16: loudness normalization (ReplayGain / EBU R128) — client offline cache.
--
-- Mirrors the server's `20271101000000_loudness.sql` so downloaded tracks carry
-- their measured loudness and the player can normalize them **offline**, exactly
-- as it does for streamed tracks. `album_loudness_lufs` is the owning album's
-- rollup (denormalized on the track server-side) for album-mode gain.
--
-- Same portability rules as the earlier migrations (nullable REALs, ids equal to
-- the server's) so the sync engine reconciles by id.
ALTER TABLE tracks ADD COLUMN loudness_lufs       REAL;
ALTER TABLE tracks ADD COLUMN loudness_peak       REAL;
ALTER TABLE tracks ADD COLUMN album_loudness_lufs REAL;
