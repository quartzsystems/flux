-- Flux initial schema.
--
-- Two conventions run through every table here:
--
--   1. Enumerations are TEXT with a CHECK constraint, not Postgres ENUM types.
--      Adding a variant is then a one-line migration instead of an ALTER TYPE
--      that cannot run inside a transaction. The permitted tokens are exactly the
--      `as_str()` values of the matching Rust enum in `flux-core::types`.
--
--   2. Configuration is a JSONB document plus whatever typed columns we actually
--      query or constrain on. The document is the serialised form of a Rust
--      struct, so the shape is defined once, in Rust.

-- ---------------------------------------------------------------------------
-- Users and sessions
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username      TEXT NOT NULL UNIQUE,
    -- Argon2id PHC string. Never a raw or reversibly encoded password.
    pw_hash       TEXT NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('admin', 'operator', 'viewer')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at TIMESTAMPTZ
);

-- Usernames are matched case-insensitively at login, so uniqueness has to be
-- enforced case-insensitively too or `Admin` and `admin` become two accounts.
CREATE UNIQUE INDEX users_username_lower_idx ON users (lower(username));

CREATE TABLE sessions (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- SHA-256 of the cookie value. Storing the hash means a database disclosure
    -- does not hand out live sessions.
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    user_agent TEXT,
    remote_ip  TEXT
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- ---------------------------------------------------------------------------
-- Ports
-- ---------------------------------------------------------------------------

CREATE TABLE port_groups (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    engine_mode TEXT NOT NULL CHECK (engine_mode IN ('stl', 'astf')),
    state       TEXT NOT NULL DEFAULT 'stopped'
                CHECK (state IN ('stopped', 'starting', 'ready', 'error')),
    -- Serialised `flux_core::config::EngineInstanceConfig`.
    trex_cfg    JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- Populated when `state = 'error'`; cleared on every successful start.
    error       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ports (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Operator-assigned label. Survives rebinds and reboots; the PCI address is
    -- the hardware identity, this is the human one.
    name        TEXT NOT NULL UNIQUE,
    pci_addr    TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    driver      TEXT,
    ifname      TEXT,
    mac         TEXT,
    speed_mbps  INTEGER,
    numa_node   INTEGER,
    mode        TEXT NOT NULL DEFAULT 'kernel' CHECK (mode IN ('kernel', 'dpdk')),
    link_state  TEXT NOT NULL DEFAULT 'unknown'
                CHECK (link_state IN ('up', 'down', 'unknown')),
    group_id    UUID REFERENCES port_groups (id) ON DELETE SET NULL,
    -- Position within the group, which is the index the engine will know it by.
    group_index SMALLINT,
    -- False once an inventory refresh stops seeing the device. The row is kept
    -- rather than deleted so a pulled card does not silently drop its name,
    -- group membership, and history.
    present     BOOLEAN NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ports_group_index_requires_group
        CHECK ((group_id IS NULL) = (group_index IS NULL))
);

CREATE UNIQUE INDEX ports_group_position_idx ON ports (group_id, group_index)
    WHERE group_id IS NOT NULL;

CREATE TABLE reservations (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    port_id    UUID NOT NULL REFERENCES ports (id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    note       TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- A port can be held by at most one person at a time.
CREATE UNIQUE INDEX reservations_one_per_port_idx ON reservations (port_id);
CREATE INDEX reservations_expires_at_idx ON reservations (expires_at);

-- ---------------------------------------------------------------------------
-- Traffic configuration
-- ---------------------------------------------------------------------------

CREATE TABLE devices (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    port_id    UUID NOT NULL REFERENCES ports (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    -- Serialised `flux_core::config::DeviceConfig`.
    config     JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (port_id, name)
);

CREATE TABLE flows (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL UNIQUE,
    config     JSONB NOT NULL,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE load_profiles (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL UNIQUE,
    config     JSONB NOT NULL,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tests (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL UNIQUE,
    type       TEXT NOT NULL CHECK (type IN (
                   'manual',
                   'rfc2544_throughput',
                   'rfc2544_latency',
                   'rfc2544_frameloss',
                   'rfc2544_b2b'
               )),
    config     JSONB NOT NULL DEFAULT '{}'::JSONB,
    flow_ids   UUID[] NOT NULL DEFAULT '{}',
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- Runs and results
-- ---------------------------------------------------------------------------

CREATE TABLE runs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Kept if the test is later deleted: a result nobody can trace back to a
    -- configuration is worthless, so the snapshot below carries the details.
    test_id     UUID REFERENCES tests (id) ON DELETE SET NULL,
    test_name   TEXT NOT NULL,
    type        TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN (
                    'pending', 'validating', 'preparing', 'running',
                    'analyzing', 'complete', 'failed', 'cancelled'
                )),
    started_by  UUID REFERENCES users (id) ON DELETE SET NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    -- Operator-supplied notes about the device under test, reproduced verbatim
    -- in the report header.
    dut_meta    JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- The complete resolved configuration at the moment the run started. This is
    -- what makes a historical result reproducible after the config has moved on.
    config_snapshot JSONB NOT NULL DEFAULT '{}'::JSONB,
    error       TEXT
);

CREATE INDEX runs_state_idx ON runs (state);
CREATE INDEX runs_test_id_idx ON runs (test_id);
CREATE INDEX runs_started_at_idx ON runs (started_at DESC);

CREATE TABLE run_results (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id     UUID NOT NULL REFERENCES runs (id) ON DELETE CASCADE,
    -- Trial number within the run, monotonic across all frame sizes.
    iteration  INTEGER NOT NULL,
    frame_size INTEGER,
    -- Trial inputs: rate, burst length, whatever the test type varies.
    params     JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- Trial outputs: tx/rx counts, loss, latency percentiles.
    metrics    JSONB NOT NULL DEFAULT '{}'::JSONB,
    -- True on the trial that established the reported result for its frame size.
    passed     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    UNIQUE (run_id, iteration)
);

CREATE INDEX run_results_run_id_idx ON run_results (run_id);

-- ---------------------------------------------------------------------------
-- Settings
-- ---------------------------------------------------------------------------

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_by UUID REFERENCES users (id) ON DELETE SET NULL
);

INSERT INTO settings (key, value) VALUES
    ('retention', '{"runDays": 90, "seriesDays": 30}'::JSONB),
    ('appliance', '{"hostname": null, "location": null}'::JSONB),
    ('tls',       '{"enabled": false}'::JSONB);
