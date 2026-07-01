-- Phase 14: persisted playlist recommendation pools (PLAYLISTS_OPTS.md Phase 4).
--
-- The in-memory RecommendationCache (Phase 3) is a hot layer over this table, so
-- a relaunch warms from disk instead of paying a cold recompute on first open.
--
-- Keyed by the CONTENT SIGNATURE (a hash of the playlist's ordered track-id set
-- + model version), NOT the playlist id: the rec read API is passed a
-- playlist's current track ids, not its id, so the signature is the natural key
-- (two playlists with the same track set share one row). Staleness is implicit
-- — a membership change or re-analysis produces a new signature, so the current
-- playlist never reads a stale row; old rows are ignored past `computed_at + TTL`
-- and can be pruned by age.
CREATE TABLE playlist_rec_cache (
    signature      TEXT        PRIMARY KEY,   -- hash of ordered track-id set + model
    model_version  TEXT        NOT NULL,
    rec_track_ids  UUID[]      NOT NULL,
    computed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Age-based pruning of superseded signatures.
CREATE INDEX idx_playlist_rec_cache_computed ON playlist_rec_cache(computed_at);
