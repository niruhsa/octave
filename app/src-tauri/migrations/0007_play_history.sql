-- Phase 11: play-history offline buffer.
--
-- A play recorded while offline (or before a flush) is queued here and pushed
-- to the server in batches by the sync scheduler (`play_history_flush`), then
-- deleted. Mirrors the `pending_ops` outbox shape. `played_at` defaults to the
-- insert time — which is when the play happened (we record at the count-as-
-- played threshold / track end), so an offline play keeps its real time when
-- the backlog is flushed later.
--
-- This is a send-only outbox: the server owns the authoritative history, so we
-- never read plays back from this table for display.
CREATE TABLE pending_plays (
    id          TEXT PRIMARY KEY,
    track_id    TEXT NOT NULL,
    ms_played   INTEGER NOT NULL DEFAULT 0,
    completed   INTEGER NOT NULL DEFAULT 0,
    played_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX idx_pending_plays_created ON pending_plays(created_at);
