-- Phase 5: Sync Engine.
--
-- Offline-edit outbox. When the server is unreachable, mutations the user
-- makes locally (playlist create/rename/delete, track add/remove/reorder)
-- are appended here as opaque op rows. On reconnect the sync engine replays
-- them in insertion order against the server, then clears the ones that
-- succeed. Server authority wins on conflict: a replayed op that the server
-- rejects is recorded with its error and dropped from the queue (the next
-- pull reconciles local state back to the server's truth).
--
-- `payload_json` is an op-specific JSON blob the sync engine knows how to
-- decode per `op_type`. Kept opaque at the schema layer so new op types
-- don't require migrations.

CREATE TABLE pending_ops (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    op_type       TEXT NOT NULL
                   CHECK (op_type IN (
                       'playlist.create',
                       'playlist.rename',
                       'playlist.delete',
                       'playlist.add_track',
                       'playlist.remove_track',
                       'playlist.reorder_track'
                   )),
    payload_json  TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT
);
CREATE INDEX idx_pending_ops_created ON pending_ops(created_at);
