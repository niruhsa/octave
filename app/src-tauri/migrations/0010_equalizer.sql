-- Phase 17: account-scoped equalizer cache, local-only layer, conflicts, and
-- a dedicated EQ mutation outbox.  The acknowledged server mirror is kept
-- separate from local/optimistic state so a snapshot can never erase offline
-- work.  Revision tokens are decimal TEXT to remain lossless through Tauri's
-- JavaScript boundary.

CREATE TABLE equalizer_synced_profiles (
    account_scope       TEXT NOT NULL,
    id                  TEXT NOT NULL,
    name                TEXT NOT NULL,
    name_key            TEXT NOT NULL,
    format_version      INTEGER NOT NULL,
    preamp_db           REAL NOT NULL,
    auto_headroom       INTEGER NOT NULL CHECK (auto_headroom IN (0, 1)),
    revision            TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    supported_v1        INTEGER NOT NULL DEFAULT 1 CHECK (supported_v1 IN (0, 1)),
    raw_payload_json    TEXT,
    PRIMARY KEY (account_scope, id),
    UNIQUE (account_scope, name_key)
);

CREATE TABLE equalizer_synced_bands (
    account_scope       TEXT NOT NULL,
    profile_id          TEXT NOT NULL,
    position            INTEGER NOT NULL,
    enabled             INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    filter_kind         TEXT NOT NULL,
    frequency_hz        REAL NOT NULL,
    gain_db             REAL NOT NULL,
    q                   REAL NOT NULL,
    PRIMARY KEY (account_scope, profile_id, position),
    FOREIGN KEY (account_scope, profile_id)
        REFERENCES equalizer_synced_profiles(account_scope, id) ON DELETE CASCADE
);

CREATE TABLE equalizer_synced_user_settings (
    account_scope       TEXT PRIMARY KEY,
    state_format_version INTEGER NOT NULL DEFAULT 1,
    state_revision      TEXT NOT NULL DEFAULT '0',
    settings_revision   TEXT NOT NULL DEFAULT '0',
    default_profile_id  TEXT,
    FOREIGN KEY (account_scope, default_profile_id)
        REFERENCES equalizer_synced_profiles(account_scope, id) ON DELETE NO ACTION
);

CREATE TABLE equalizer_synced_device_rules (
    account_scope       TEXT NOT NULL,
    id                  TEXT NOT NULL,
    label               TEXT NOT NULL,
    action              TEXT NOT NULL CHECK (action IN ('profile', 'bypass')),
    profile_id          TEXT,
    selectors_json      TEXT NOT NULL,
    priority            INTEGER NOT NULL,
    enabled             INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    revision            TEXT NOT NULL,
    supported_v1        INTEGER NOT NULL DEFAULT 1 CHECK (supported_v1 IN (0, 1)),
    raw_payload_json    TEXT,
    PRIMARY KEY (account_scope, id),
    CHECK ((action = 'profile' AND profile_id IS NOT NULL)
        OR (action = 'bypass' AND profile_id IS NULL)),
    FOREIGN KEY (account_scope, profile_id)
        REFERENCES equalizer_synced_profiles(account_scope, id) ON DELETE NO ACTION
);

CREATE INDEX idx_eq_synced_rules_priority
    ON equalizer_synced_device_rules(account_scope, priority DESC, id);

CREATE TABLE equalizer_local_profiles (
    account_scope       TEXT NOT NULL,
    id                  TEXT NOT NULL,
    name                TEXT NOT NULL,
    name_key            TEXT NOT NULL,
    format_version      INTEGER NOT NULL DEFAULT 1,
    preamp_db           REAL NOT NULL,
    auto_headroom       INTEGER NOT NULL CHECK (auto_headroom IN (0, 1)),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    PRIMARY KEY (account_scope, id),
    UNIQUE (account_scope, name_key)
);

CREATE TABLE equalizer_local_bands (
    account_scope       TEXT NOT NULL,
    profile_id          TEXT NOT NULL,
    position            INTEGER NOT NULL,
    enabled             INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    filter_kind         TEXT NOT NULL,
    frequency_hz        REAL NOT NULL,
    gain_db             REAL NOT NULL,
    q                   REAL NOT NULL,
    PRIMARY KEY (account_scope, profile_id, position),
    FOREIGN KEY (account_scope, profile_id)
        REFERENCES equalizer_local_profiles(account_scope, id) ON DELETE CASCADE
);

CREATE TABLE equalizer_local_user_settings (
    account_scope       TEXT PRIMARY KEY,
    default_profile_id  TEXT,
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    FOREIGN KEY (account_scope, default_profile_id)
        REFERENCES equalizer_local_profiles(account_scope, id) ON DELETE NO ACTION
);

CREATE TABLE equalizer_local_device_rules (
    account_scope       TEXT NOT NULL,
    id                  TEXT NOT NULL,
    label               TEXT NOT NULL,
    action              TEXT NOT NULL CHECK (action IN ('profile', 'bypass')),
    profile_id          TEXT,
    selectors_json      TEXT NOT NULL,
    priority            INTEGER NOT NULL,
    enabled             INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    PRIMARY KEY (account_scope, id),
    CHECK ((action = 'profile' AND profile_id IS NOT NULL)
        OR (action = 'bypass' AND profile_id IS NULL)),
    FOREIGN KEY (account_scope, profile_id)
        REFERENCES equalizer_local_profiles(account_scope, id) ON DELETE NO ACTION
);

CREATE INDEX idx_eq_local_rules_priority
    ON equalizer_local_device_rules(account_scope, priority DESC, id);

CREATE TABLE equalizer_local_preferences (
    account_scope       TEXT PRIMARY KEY,
    master_enabled      INTEGER NOT NULL DEFAULT 0 CHECK (master_enabled IN (0, 1)),
    automatic_switching_enabled INTEGER NOT NULL DEFAULT 0
        CHECK (automatic_switching_enabled IN (0, 1)),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE equalizer_sync_state (
    account_scope       TEXT PRIMARY KEY,
    state_revision      TEXT NOT NULL DEFAULT '0',
    etag                TEXT,
    has_complete_snapshot INTEGER NOT NULL DEFAULT 0 CHECK (has_complete_snapshot IN (0, 1)),
    support_state       TEXT NOT NULL DEFAULT 'unknown'
        CHECK (support_state IN ('unknown', 'supported', 'unsupported', 'future_format')),
    last_probe_at       TEXT,
    last_probe_app_version TEXT,
    synced_at           TEXT
);

CREATE TABLE equalizer_local_device_overrides (
    account_scope       TEXT NOT NULL,
    endpoint_key        TEXT NOT NULL,
    display_label       TEXT,
    target_layer        TEXT NOT NULL CHECK (target_layer IN ('synced', 'local_only')),
    action              TEXT NOT NULL CHECK (action IN ('profile', 'bypass')),
    profile_id          TEXT,
    orphaned            INTEGER NOT NULL DEFAULT 0 CHECK (orphaned IN (0, 1)),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (account_scope, endpoint_key),
    CHECK ((action = 'profile' AND profile_id IS NOT NULL)
        OR (action = 'bypass' AND profile_id IS NULL))
);

CREATE TABLE equalizer_conflicts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    account_scope       TEXT NOT NULL,
    dependency_group    TEXT NOT NULL,
    op_type             TEXT NOT NULL,
    entity_id           TEXT,
    payload_json        TEXT NOT NULL,
    base_revision       TEXT,
    server_revision     TEXT,
    error_code          TEXT NOT NULL,
    error_message       TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_eq_conflicts_scope
    ON equalizer_conflicts(account_scope, id);

CREATE TABLE pending_equalizer_ops (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_uuid      TEXT NOT NULL UNIQUE,
    account_scope       TEXT NOT NULL,
    op_type             TEXT NOT NULL,
    entity_id           TEXT,
    base_revision       TEXT,
    dependency_group    TEXT NOT NULL,
    payload_json        TEXT NOT NULL,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    attempts            INTEGER NOT NULL DEFAULT 0,
    last_error          TEXT
);

CREATE INDEX idx_pending_eq_scope_fifo
    ON pending_equalizer_ops(account_scope, id);
