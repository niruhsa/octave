-- Phase: Podcast episode playback progress.
--
-- Per-user, per-episode playback position so a listener can resume where they
-- left off, and a "completed" flag marking an episode as fully listened. Mirrors
-- the per-user shape of `podcast_subscriptions`; same portability rules (UUID/
-- BIGINT/BOOLEAN/TIMESTAMPTZ) so the client's SQLite cache mirrors it 1:1.
CREATE TABLE podcast_episode_progress (
    user_id     UUID        NOT NULL REFERENCES users(id)            ON DELETE CASCADE,
    episode_id  UUID        NOT NULL REFERENCES podcast_episodes(id) ON DELETE CASCADE,
    -- Last known playback position. 0 = start. Held even once completed so a
    -- finished episode can still show how far the bar reached.
    position_ms BIGINT      NOT NULL DEFAULT 0,
    -- TRUE once the episode was played to (near) its end.
    completed   BOOLEAN     NOT NULL DEFAULT FALSE,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, episode_id)
);
CREATE INDEX idx_episode_progress_user ON podcast_episode_progress(user_id);
