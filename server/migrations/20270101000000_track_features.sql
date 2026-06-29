-- Phase 12: acoustic similarity embeddings.
--
-- One embedding per track, server-only. NOT mirrored to the client SQLite cache
-- (embeddings are large + irrelevant offline) — a deliberate exception to the
-- portability rule. The vector is stored as a raw little-endian f32 BYTEA blob
-- (portable, no Postgres extension); `dims` + `model_version` make it
-- self-describing so a model change can re-analyze only what's stale.
CREATE TABLE track_features (
    track_id      UUID        PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    -- The similarity embedding: `dims` little-endian f32s.
    embedding     BYTEA       NOT NULL,
    dims          INT         NOT NULL,
    -- Which extractor produced it ("dsp-v1", "openl3-512", ...). A bump
    -- invalidates old rows so the pass re-analyzes them.
    model_version TEXT        NOT NULL,
    -- File-content signature at analysis time (size+mtime or a hash) so a
    -- re-encoded/replaced file is re-analyzed. Mirrors the image-opt freshness check.
    source_sig    TEXT        NOT NULL,
    -- Optional identification fingerprint (Chromaprint) — §9, nullable until/if added.
    chromaprint   TEXT,
    analyzed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Drives the "which tracks still need analysis / are stale" scan.
CREATE INDEX idx_track_features_model ON track_features(model_version);
