-- Phase 15: offline lyrics mirror.
--
-- Unlike acoustic embeddings, lyrics ARE mirrored to the client — tiny text
-- that's most useful offline (a downloaded track on a plane). The download
-- flow fetches `/tracks/:id/lyrics` and persists it here; an online read also
-- refreshes it. Standalone (no FK) so a refresh-on-read for a not-yet-cached
-- track is safe. `lines_json` holds a JSON array of `{ms,text}` (parsed
-- server-side); `synced` distinguishes timed lines from a plain-text dump;
-- `instrumental` records a positive "no lyrics" so the panel shows the right
-- state offline.
CREATE TABLE track_lyrics (
    track_id     TEXT PRIMARY KEY,
    found        INTEGER NOT NULL DEFAULT 0,
    synced       INTEGER NOT NULL DEFAULT 0,
    instrumental INTEGER NOT NULL DEFAULT 0,
    source       TEXT,
    lines_json   TEXT NOT NULL DEFAULT '[]',
    plain        TEXT NOT NULL DEFAULT '',
    fetched_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
