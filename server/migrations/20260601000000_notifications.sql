-- Phase 10: follow notifications.
--
-- A notification is delivered to a single user. `kind` is free TEXT (not a
-- CHECK-constrained enum) so future notification kinds need no migration; the
-- only kind today is 'new_release'.
--
-- `artist_id` / `album_id` are nullable references that go NULL (not cascade-
-- delete) when the entity is removed, so a user's notification history survives
-- a later catalog deletion. The denormalized `title` / `body` keep the
-- notification meaningful even after the referenced rows are gone.
--
-- Portable to the client's SQLite cache (UUID/TEXT/TIMESTAMPTZ, partial index).

CREATE TABLE notifications (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID        NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    kind        TEXT        NOT NULL,                       -- e.g. 'new_release'
    artist_id   UUID        REFERENCES artists(id)          ON DELETE SET NULL,
    album_id    UUID        REFERENCES albums(id)           ON DELETE SET NULL,
    -- Denormalized display text so the notification reads correctly even after
    -- the referenced artist/album is edited or deleted.
    title       TEXT        NOT NULL,
    body        TEXT,
    read_at     TIMESTAMPTZ,                                -- NULL = unread
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-first listing per user.
CREATE INDEX idx_notifications_user ON notifications(user_id, created_at DESC);
-- Fast unread count / unread-only listing.
CREATE INDEX idx_notifications_unread ON notifications(user_id) WHERE read_at IS NULL;
