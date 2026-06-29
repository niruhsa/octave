-- Phase 11: favorites (track / album / artist).
--
-- A favorite is a per-user "like" on one catalog entity. Polymorphic over the
-- three entity kinds via three nullable FKs (exactly one set, enforced by the
-- CHECK) rather than a (type, id) text pair — so a favorite is cascade-deleted
-- with its entity and the FKs stay real. Mirrors the `notifications` nullable-FK
-- pattern. Any authed *user* may favorite; the SECRET_KEY identity has no user
-- and is rejected at the service layer.
--
-- Portable to the client's SQLite cache (UUID/TIMESTAMPTZ + partial indexes).
CREATE TABLE favorites (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    track_id   UUID        REFERENCES tracks(id)           ON DELETE CASCADE,
    album_id   UUID        REFERENCES albums(id)           ON DELETE CASCADE,
    artist_id  UUID        REFERENCES artists(id)          ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(track_id, album_id, artist_id) = 1)
);

-- One favorite per (user, entity); also the fast "is favorited?" lookup.
CREATE UNIQUE INDEX uq_fav_track  ON favorites(user_id, track_id)  WHERE track_id  IS NOT NULL;
CREATE UNIQUE INDEX uq_fav_album  ON favorites(user_id, album_id)  WHERE album_id  IS NOT NULL;
CREATE UNIQUE INDEX uq_fav_artist ON favorites(user_id, artist_id) WHERE artist_id IS NOT NULL;
-- Newest-first listing per user.
CREATE INDEX idx_favorites_user ON favorites(user_id, created_at DESC);
