-- Pausable uploads: add the `paused` session state.
--
-- An upload can be paused (manually by the user, or automatically by the client
-- when chunk uploads stall/fail for >= 1 minute) and later resumed (manually, or
-- automatically when a chunk lands again). `paused` is an *active* state: it
-- counts toward the one-active-upload-per-user limit and is cancellable.
--
-- This widens the CHECK on `uploads.state`. The original inline column CHECK in
-- 20260201000000_uploads.sql is auto-named `uploads_state_check` by Postgres.

ALTER TABLE uploads DROP CONSTRAINT uploads_state_check;
ALTER TABLE uploads ADD CONSTRAINT uploads_state_check
    CHECK (state IN ('initialized', 'uploading', 'paused', 'completed', 'cancelled'));
