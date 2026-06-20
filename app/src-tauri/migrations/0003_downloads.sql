-- Phase 6: Offline Downloads.
--
-- A tiny key/value settings store for client-only preferences that don't
-- belong on a cached server entity: the downloads root override and the
-- mobile "Wi-Fi only" toggle. Values are opaque TEXT; callers parse.
--
-- The download file rows themselves reuse the existing `tracks.local_file_path`
-- + `album_art.local_cover_path` columns — presence of a row is the source
-- of truth for "downloaded", same as Phase 1.

CREATE TABLE settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
