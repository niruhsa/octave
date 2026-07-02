-- Phase 14 (Phase D): make the discography provider ids provider-agnostic so a
-- non-UUID provider (Discogs) works. This supersedes the UUID-typed columns
-- created in 20270801 (which is left immutable, per the append-only migration
-- rule). Existing MusicBrainz data is preserved: a stored MBID becomes a
-- `('musicbrainz', <mbid-as-text>)` pair, and the UUID ignore ids become their
-- canonical text form (which is exactly what the provider snapshot stores, so
-- existing ignores keep matching).

-- artist_discography: `mbid UUID` -> `provider` + `provider_id` (TEXT). Written
-- idempotently (guards + a conditional block) so it's safe regardless of the
-- exact starting state.
ALTER TABLE artist_discography ADD COLUMN IF NOT EXISTS provider    TEXT;
ALTER TABLE artist_discography ADD COLUMN IF NOT EXISTS provider_id TEXT;
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'artist_discography' AND column_name = 'mbid'
    ) THEN
        UPDATE artist_discography
            SET provider = 'musicbrainz', provider_id = mbid::text
            WHERE mbid IS NOT NULL;
        ALTER TABLE artist_discography DROP COLUMN mbid;
    END IF;
END $$;

-- discography_ignores: UUID ids -> TEXT. The track-scope unique index's
-- COALESCE sentinel is a UUID literal, so it must be dropped before the retype
-- and rebuilt with a text sentinel. (The release-scope index is on plain
-- columns and is rebuilt automatically by the retype.)
DROP INDEX IF EXISTS uq_disco_ignore_track;
ALTER TABLE discography_ignores
    ALTER COLUMN release_group_id TYPE TEXT USING release_group_id::text;
ALTER TABLE discography_ignores
    ALTER COLUMN recording_id TYPE TEXT USING recording_id::text;
CREATE UNIQUE INDEX uq_disco_ignore_track
    ON discography_ignores(
        artist_id, release_group_id,
        COALESCE(recording_id, ''),
        COALESCE(title_key, ''))
    WHERE scope = 'track';
