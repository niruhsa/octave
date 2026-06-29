-- Phase 11: play history.
--
-- One row per "play" — the foundation for listening stats, "recently played",
-- and behavioral recommendations. The client decides *when* a play counts
-- (e.g. ≥30 s OR ≥50 % of the track) and posts the event; the server stores it.
--
-- `track_id`/`artist_id`/`album_id` are nullable references that go NULL (not
-- cascade-delete) when the catalog row is removed, so a user's listening record
-- survives a later deletion. The denormalized `track_title`/`artist_name` keep
-- the history readable even after the referenced rows are gone — same rule as
-- `notifications`.
--
-- `played_at` is client-supplied (offline plays keep their real time when the
-- backlog is flushed on reconnect); it defaults to now() when omitted. Portable
-- to the client's SQLite cache (UUID/TEXT/BIGINT/BOOLEAN/TIMESTAMPTZ).
CREATE TABLE play_events (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID        NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    track_id     UUID        REFERENCES tracks(id)           ON DELETE SET NULL,
    artist_id    UUID        REFERENCES artists(id)          ON DELETE SET NULL,
    album_id     UUID        REFERENCES albums(id)           ON DELETE SET NULL,
    -- Denormalized display text so a play reads correctly after a catalog edit.
    track_title  TEXT        NOT NULL,
    artist_name  TEXT        NOT NULL,
    -- How far the listener got. Lets a skip be told apart from a real play.
    ms_played    BIGINT      NOT NULL DEFAULT 0,
    completed    BOOLEAN     NOT NULL DEFAULT FALSE,
    played_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-first listing per user (recently played).
CREATE INDEX idx_play_events_user  ON play_events(user_id, played_at DESC);
-- Per-track aggregation (play counts, top tracks).
CREATE INDEX idx_play_events_track ON play_events(track_id);
-- Per-user-per-artist aggregation (top artists / window stats).
CREATE INDEX idx_play_events_user_artist ON play_events(user_id, artist_id);
