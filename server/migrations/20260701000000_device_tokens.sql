-- Phase 10: device push tokens (Firebase Cloud Messaging).
--
-- One row per (device) push token, owned by a user. The token is the primary
-- key — it is globally unique and, on reinstall / a different login, an upsert
-- reassigns it to the current user. `platform` is free TEXT ('android' today;
-- 'ios'/'web' later). Cascade-deletes with the user.
--
-- Portable to the client's SQLite cache (UUID/TEXT/TIMESTAMPTZ), though the
-- client never stores these — they're server-side delivery state.

CREATE TABLE device_tokens (
    token        TEXT        PRIMARY KEY,
    user_id      UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform     TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Fan-out looks tokens up by recipient user.
CREATE INDEX idx_device_tokens_user ON device_tokens(user_id);
