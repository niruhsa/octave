-- Phase: Podcast episode playback progress (client offline cache).
--
-- Single-user mirror of the server's `20261001000000_episode_progress.sql` (no
-- user_id — the client is one account). Holds the last playback position and a
-- "completed" flag per episode so the player can resume where the listener left
-- off and the list can mark episodes listened, online or off. Standalone (no FK)
-- so progress can be recorded/synced even if an episode's metadata row was
-- evicted; the episode list LEFT JOINs by episode_id, so orphans are harmless.
CREATE TABLE podcast_episode_progress (
    episode_id  TEXT PRIMARY KEY,
    position_ms INTEGER NOT NULL DEFAULT 0,
    completed   INTEGER NOT NULL DEFAULT 0,    -- 1 = played to (near) the end
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
