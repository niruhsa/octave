-- Phase 14: discography sync (external metadata reconciliation).
--
-- Reconcile each artist against MusicBrainz so managers can see which
-- albums/EPs/singles the library is missing and, for owned releases, which
-- tracks are missing. See DISCOGRAPHY_SYNC.md.
--
-- Design note: resolution state + reports live in SIDE TABLES keyed by
-- artist_id (not new columns on `artists`/`albums`). The repo layer maps every
-- row with explicit column lists + `FromRow`, so adding columns to the shared
-- Artist/Album structs would force every existing query to change; a side table
-- keeps that blast radius at zero. JSON payloads are stored as TEXT (the
-- codebase-wide convention — cf. metadata_json / report_json / categories).
-- All three tables are SERVER-ONLY: a manager tool, never mirrored into the
-- client SQLite cache.

-- Per-artist provider resolution state. Provider-agnostic (Phase D): `provider`
-- names the metadata source (`musicbrainz` / `discogs`) and `provider_id` is the
-- artist's id ON that provider — a MusicBrainz MBID, a Discogs artist id, etc.
-- Stored as TEXT (not UUID) so non-UUID providers work. Sticky: set once, reused
-- by later syncs; a provider switch makes a stored id stale (provider mismatch)
-- so the artist is re-resolved. `match_status`:
--   unresolved  no confident match yet (needs a manager to disambiguate)
--   matched     auto-accepted a high-confidence provider match
--   manual      a manager pinned the match by hand
--   ignored     the artist is excluded from reconciliation entirely
CREATE TABLE artist_discography (
    artist_id    UUID        PRIMARY KEY REFERENCES artists(id) ON DELETE CASCADE,
    provider     TEXT,
    provider_id  TEXT,
    match_status TEXT        NOT NULL DEFAULT 'unresolved'
        CHECK (match_status IN ('unresolved','matched','manual','ignored')),
    synced_at    TIMESTAMPTZ
);

-- One cached gap report per artist. `provider_snapshot` is the raw, UNFILTERED
-- diff from the last sync (every release-group + each owned album's pre-ignore
-- missing-track list); it lets ignore/unignore re-filter in memory without
-- re-hitting MusicBrainz. `missing_releases` / `incomplete_albums` are the
-- filtered payloads the UI renders. All three are JSON-as-TEXT.
CREATE TABLE discography_reports (
    artist_id              UUID        PRIMARY KEY REFERENCES artists(id) ON DELETE CASCADE,
    provider               TEXT        NOT NULL,
    missing_releases       TEXT        NOT NULL,
    incomplete_albums      TEXT        NOT NULL,
    provider_snapshot      TEXT        NOT NULL,
    missing_release_count  INT         NOT NULL,
    incomplete_album_count INT         NOT NULL,
    generated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Suppression list (DISCOGRAPHY_SYNC.md §4.7): releases / tracks a manager has
-- chosen to ignore, so a gap they don't want stops being reported on every
-- sync. Keyed on PROVIDER ids (stable across library edits + re-matching),
-- scoped per artist, reversible.
CREATE TABLE discography_ignores (
    id               UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    artist_id        UUID        NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    scope            TEXT        NOT NULL CHECK (scope IN ('release','track')),
    -- Provider release-group id the ignored item belongs to (both scopes). TEXT
    -- so non-UUID providers (Discogs) work; provider ids are namespaced by value
    -- so an ignore from one provider never matches another's snapshot.
    release_group_id TEXT        NOT NULL,
    -- scope='track': provider recording id when supplied (else NULL, and
    -- `title_key` is the match key). Unused for scope='release'.
    recording_id     TEXT,
    -- Normalized title (§4.3): the fallback match key for a title-based track
    -- ignore. NULL for scope='release'.
    title_key        TEXT,
    -- Human-readable label shown in the "Ignored" management view.
    label            TEXT        NOT NULL,
    created_by       UUID        REFERENCES users(id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Idempotency: one ignore per release, and per track within a release. The
-- COALESCE sentinels let (recording_id, title_key) be either-or without NULLs
-- defeating the unique constraint.
CREATE UNIQUE INDEX uq_disco_ignore_release
    ON discography_ignores(artist_id, release_group_id)
    WHERE scope = 'release';
CREATE UNIQUE INDEX uq_disco_ignore_track
    ON discography_ignores(
        artist_id, release_group_id,
        COALESCE(recording_id, ''),
        COALESCE(title_key, ''))
    WHERE scope = 'track';
CREATE INDEX idx_disco_ignore_artist ON discography_ignores(artist_id);
CREATE INDEX idx_artist_discography_status ON artist_discography(match_status);
