-- Add a fourth album classification: Live.
--
-- Extends the type set introduced in 20270201000000_album_type.sql
-- (Album / EP / Single) with `live` for concert / live-recording releases.
-- Like `album` and `ep` (and unlike `single`), a `live` album carries no
-- required-single-song invariant — it's just a classification.
--
-- The original column defined its CHECK inline, so Postgres named it
-- `albums_album_type_check`. Drop that and re-add the widened set. Existing
-- rows are unaffected (every current value is still valid).

ALTER TABLE albums DROP CONSTRAINT IF EXISTS albums_album_type_check;
ALTER TABLE albums
    ADD CONSTRAINT albums_album_type_check
    CHECK (album_type IN ('album', 'ep', 'single', 'live'));
