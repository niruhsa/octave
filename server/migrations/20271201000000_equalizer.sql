-- Cross-device synced parametric equalizer configuration. The server stores
-- configuration only; audio processing remains entirely client-side.

CREATE TABLE equalizer_profiles (
    id                    UUID             PRIMARY KEY,
    owner_id              UUID             NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                  TEXT             NOT NULL,
    name_key              TEXT             NOT NULL,
    format_version        INTEGER          NOT NULL CHECK (format_version = 1),
    preamp_db             DOUBLE PRECISION NOT NULL CHECK (preamp_db >= -30 AND preamp_db <= 12),
    auto_headroom_enabled BOOLEAN          NOT NULL DEFAULT TRUE,
    revision              BIGINT           NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at            TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ      NOT NULL DEFAULT now(),
    UNIQUE (owner_id, id),
    UNIQUE (owner_id, name_key)
);
CREATE INDEX idx_equalizer_profiles_owner ON equalizer_profiles(owner_id);

CREATE TABLE equalizer_bands (
    profile_id   UUID             NOT NULL REFERENCES equalizer_profiles(id) ON DELETE CASCADE,
    position     INTEGER          NOT NULL CHECK (position >= 1 AND position <= 32),
    enabled      BOOLEAN          NOT NULL,
    filter_type  TEXT             NOT NULL CHECK (filter_type = 'peaking'),
    frequency_hz DOUBLE PRECISION NOT NULL CHECK (frequency_hz >= 10 AND frequency_hz <= 20000),
    gain_db      DOUBLE PRECISION NOT NULL CHECK (gain_db >= -24 AND gain_db <= 24),
    q            DOUBLE PRECISION NOT NULL CHECK (q >= 0.1 AND q <= 30),
    PRIMARY KEY (profile_id, position)
);

CREATE TABLE equalizer_user_settings (
    user_id            UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    default_profile_id UUID,
    revision           BIGINT      NOT NULL DEFAULT 0 CHECK (revision >= 0),
    state_revision     BIGINT      NOT NULL DEFAULT 0 CHECK (state_revision >= 0),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_equalizer_default_owner
        FOREIGN KEY (user_id, default_profile_id)
        REFERENCES equalizer_profiles(owner_id, id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE equalizer_device_rules (
    id             UUID        PRIMARY KEY,
    owner_id       UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    profile_id     UUID,
    action         TEXT        NOT NULL CHECK (action IN ('profile', 'bypass')),
    label          TEXT        NOT NULL,
    selector_json  TEXT        NOT NULL CHECK (octet_length(selector_json) <= 8192),
    selector_hash  TEXT        NOT NULL CHECK (char_length(selector_hash) = 64),
    priority       INTEGER     NOT NULL,
    enabled        BOOLEAN     NOT NULL DEFAULT TRUE,
    revision       BIGINT      NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT ck_equalizer_rule_action_profile CHECK (
        (action = 'profile' AND profile_id IS NOT NULL)
        OR (action = 'bypass' AND profile_id IS NULL)
    ),
    CONSTRAINT fk_equalizer_rule_profile_owner
        FOREIGN KEY (owner_id, profile_id)
        REFERENCES equalizer_profiles(owner_id, id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    UNIQUE (owner_id, id),
    UNIQUE (owner_id, selector_hash)
);
CREATE INDEX idx_equalizer_rules_owner_priority
    ON equalizer_device_rules(owner_id, priority DESC, id);

CREATE INDEX idx_audit_equalizer_changes
    ON audit_log(created_at DESC, id)
    WHERE entity_type = 'equalizer_state';
