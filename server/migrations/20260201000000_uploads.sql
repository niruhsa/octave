-- Uploads v2: persistent, resumable, verifiable upload sessions.
--
-- Replaces the disk-only chunked upload (manifest.json/.part/result.json) with
-- a DB-backed model that survives restarts, is queryable as a report, verifies
-- every chunk by hash, and drives live progress broadcasts.
--
-- An "upload" is a SESSION that carries one or more files (a single audio file
-- or archive is a 1-file session; a multi-select / folder upload is an N-file
-- session). One session -> one report (`GET /uploads/:id`).
--
-- Portable to the client SQLite mirror, like 20260101000000_init.sql:
-- UUIDs, TEXT+CHECK enums, JSON-as-TEXT, no PG-only types.

-- ---------------------------------------------------------------------------
-- uploads (the session / report)
-- ---------------------------------------------------------------------------
CREATE TABLE uploads (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Owner. NULL for SECRET_KEY-authenticated (system/admin) uploads, mirroring
    -- audit_log.actor_id. Admins see all; users see only their own.
    user_id      UUID        REFERENCES users(id) ON DELETE CASCADE,
    state        TEXT        NOT NULL DEFAULT 'initialized'
                             CHECK (state IN ('initialized', 'uploading', 'completed', 'cancelled')),
    total_files  INTEGER     NOT NULL,
    total_bytes  BIGINT      NOT NULL,
    -- Aggregated ingest report (per-file results + totals). NULL until completed.
    report_json  TEXT,
    -- Last fatal error, if any (e.g. reassembly failure). NULL otherwise.
    error        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_uploads_user    ON uploads(user_id);
CREATE INDEX idx_uploads_state   ON uploads(state);
CREATE INDEX idx_uploads_created ON uploads(created_at);

-- ---------------------------------------------------------------------------
-- upload_files (one row per file in a session)
-- ---------------------------------------------------------------------------
CREATE TABLE upload_files (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    upload_id       UUID        NOT NULL REFERENCES uploads(id) ON DELETE CASCADE,
    -- 0-based order within the session; used in the on-disk staging path.
    file_index      INTEGER     NOT NULL,
    -- Output filename the recombined chunks are written to (drives ingest +
    -- archive-format detection). e.g. "album.tar.gz", "01 - Song.flac".
    filename        TEXT        NOT NULL,
    -- Expected SHA-256 (lowercase hex) of the fully reassembled file.
    file_hash       TEXT        NOT NULL,
    total_size      BIGINT      NOT NULL,
    chunk_size      BIGINT      NOT NULL,
    total_chunks    INTEGER     NOT NULL,
    received_chunks INTEGER     NOT NULL DEFAULT 0,
    state           TEXT        NOT NULL DEFAULT 'pending'
                                CHECK (state IN ('pending', 'uploading', 'complete', 'failed')),
    error           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (upload_id, file_index)
);
CREATE INDEX idx_upload_files_upload ON upload_files(upload_id);

-- ---------------------------------------------------------------------------
-- upload_chunks (one row per chunk per file)
-- ---------------------------------------------------------------------------
-- Relational (not JSON-on-file) so concurrent chunk POSTs update independent
-- rows with no read-modify-write race, and admins can browse exactly which
-- chunks have landed. Row count is bounded by total_size / chunk_size.
CREATE TABLE upload_chunks (
    upload_file_id UUID        NOT NULL REFERENCES upload_files(id) ON DELETE CASCADE,
    chunk_index    INTEGER     NOT NULL,
    start_byte     BIGINT      NOT NULL,
    end_byte       BIGINT      NOT NULL,
    -- Expected SHA-256 (lowercase hex) of this chunk's content. Recomputed and
    -- compared on upload; a mismatch fails that chunk (no state change).
    hash           TEXT        NOT NULL,
    received       BOOLEAN     NOT NULL DEFAULT FALSE,
    received_at    TIMESTAMPTZ,
    PRIMARY KEY (upload_file_id, chunk_index)
);
