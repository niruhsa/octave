-- Phase: Podcasts.
--
-- A podcast is a catalog show (like an artist) whose episodes are on-disk
-- audio files (like tracks). Episodes are served by the same byte-range
-- streaming as the music library; new-episode alerts reuse the notification
-- fan-out. Same portability rules as every prior migration (UUID/TEXT/CHECK/
-- JSON-as-TEXT/TIMESTAMPTZ) so the client's SQLite cache mirrors it 1:1.

-- ---------------------------------------------------------------------------
-- podcasts: one row per subscribed show (RSS feed). `feed_url` is the natural
-- key (a feed is a feed); `itunes_id` / `podcastindex_id` link back to the
-- directory that surfaced it.
-- ---------------------------------------------------------------------------
CREATE TABLE podcasts (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    feed_url          TEXT        NOT NULL UNIQUE,
    title             TEXT        NOT NULL,
    author            TEXT,                       -- itunes:author / managingEditor
    description       TEXT,
    image_path        TEXT,                       -- cached cover, like albums.cover_path
    image_url         TEXT,                       -- remote artwork URL (pre-cache)
    link              TEXT,                       -- show homepage
    language          TEXT,                       -- feed <language>
    categories        TEXT        NOT NULL DEFAULT '[]',  -- JSON array as TEXT
    itunes_id         BIGINT,                     -- iTunes collectionId (nullable)
    podcastindex_id   BIGINT,                     -- PodcastIndex feed id (nullable)
    -- Per-show auto-download policy. 0 = metadata only (stream on demand);
    -- N = auto-download the newest N episodes on each refresh.
    auto_download     INTEGER     NOT NULL DEFAULT 0,
    last_refreshed_at TIMESTAMPTZ,                -- NULL = never refreshed
    last_etag         TEXT,                       -- HTTP ETag for conditional GET
    last_modified     TEXT,                       -- HTTP Last-Modified for conditional GET
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_podcasts_title ON podcasts(title);

-- ---------------------------------------------------------------------------
-- podcast_episodes: one row per <item> in the feed. Mirrors `tracks` so the
-- streaming + download machinery is reusable. `file_path` is NULL until the
-- audio is downloaded to disk; `enclosure_url` is the remote source.
-- ---------------------------------------------------------------------------
CREATE TABLE podcast_episodes (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    podcast_id     UUID        NOT NULL REFERENCES podcasts(id) ON DELETE CASCADE,
    guid           TEXT        NOT NULL,          -- feed <guid> (episode identity)
    title          TEXT        NOT NULL,
    description    TEXT,
    enclosure_url  TEXT        NOT NULL,          -- remote audio URL
    enclosure_type TEXT,                          -- e.g. audio/mpeg
    episode_no     INTEGER,                       -- itunes:episode
    season_no      INTEGER,                       -- itunes:season
    duration_ms    BIGINT,                        -- itunes:duration (or measured on download)
    codec          TEXT,                          -- known after download/probe
    bitrate_kbps   INTEGER,
    file_path      TEXT        UNIQUE,            -- on-disk path; NULL until downloaded
    file_size      BIGINT,
    image_path     TEXT,                          -- per-episode art (optional)
    published_at   TIMESTAMPTZ,                   -- <pubDate>
    metadata_json  TEXT        NOT NULL DEFAULT '{}',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (podcast_id, guid)                     -- a guid is unique within a feed
);
CREATE INDEX idx_episodes_podcast   ON podcast_episodes(podcast_id, published_at DESC);
CREATE INDEX idx_episodes_published ON podcast_episodes(published_at DESC);

-- ---------------------------------------------------------------------------
-- podcast_subscriptions: a user follows a show for new-episode notifications.
-- Mirrors `follows` (user -> artist) exactly. Distinct from the catalog: a
-- Manager adds the show; any user may subscribe to be alerted.
-- ---------------------------------------------------------------------------
CREATE TABLE podcast_subscriptions (
    user_id    UUID        NOT NULL REFERENCES users(id)    ON DELETE CASCADE,
    podcast_id UUID        NOT NULL REFERENCES podcasts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, podcast_id)
);
CREATE INDEX idx_podcast_subs_podcast ON podcast_subscriptions(podcast_id);

-- ---------------------------------------------------------------------------
-- Extend notifications to reference a podcast/episode. `kind` is already free
-- TEXT (no CHECK), so `'new_episode'` needs no enum change — but the existing
-- artist_id/album_id columns don't fit a podcast, so add two nullable refs that
-- go NULL (not cascade) on delete, exactly like artist_id/album_id.
-- ---------------------------------------------------------------------------
ALTER TABLE notifications
    ADD COLUMN podcast_id UUID REFERENCES podcasts(id)         ON DELETE SET NULL,
    ADD COLUMN episode_id UUID REFERENCES podcast_episodes(id) ON DELETE SET NULL;
