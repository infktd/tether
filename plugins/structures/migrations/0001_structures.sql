-- Structures' own schema (the plugin's; the host runs this once).

-- One row of settings.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    -- The Discord channels (assigned ones) each kind of notification goes
    -- to; none means not sent.
    attack_channel text,
    fuel_channel text,
    state_channel text,
    moon_channel text,
    -- Hours of fuel left at which Tether sends its own low-fuel alert,
    -- largest first, e.g. '72,24,6'.
    fuel_thresholds text NOT NULL DEFAULT '72,24,6'
        CHECK (fuel_thresholds ~ '^[0-9]{1,4}(,[0-9]{1,4}){0,4}$'),
    -- Mention the Discord role mapped to Member on attacks.
    mention_members boolean NOT NULL DEFAULT false,
    -- Since when the host has listed no data sources while owners were
    -- known (a hiccup is ridden out for an hour).
    sources_missing_since timestamptz
);
INSERT INTO settings DEFAULT VALUES;

-- Structure owners: the approved data sources (AA's "Add Structure
-- Owner"), each reading its corporation's structures and its own
-- notifications. Kept in step with the host's list by the sync.
CREATE TABLE owners (
    character_id bigint PRIMARY KEY,
    character_name text NOT NULL,
    corporation_id bigint NOT NULL,
    alliance_id bigint,
    -- Last good read, failures in a row, and no calls before this (ESI
    -- said 403: the role is gone; or the token is).
    structures_at timestamptz,
    structures_failures integer NOT NULL DEFAULT 0,
    structures_retry_at timestamptz,
    notifications_at timestamptz,
    notifications_failures integer NOT NULL DEFAULT 0,
    notifications_retry_at timestamptz,
    last_error text
);
CREATE INDEX owners_corporation_idx ON owners (corporation_id);

-- The owners' structures, from ESI (replaced whole on each read).
CREATE TABLE structures (
    structure_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    name text NOT NULL,
    type_id bigint NOT NULL,
    system_id bigint NOT NULL,
    fuel_expires timestamptz,
    state text NOT NULL,
    state_timer_start timestamptz,
    state_timer_end timestamptz,
    unanchors_at timestamptz,
    reinforce_hour integer,
    next_reinforce_hour integer,
    next_reinforce_apply timestamptz,
    -- [{"name": ..., "state": "online" | "offline" | "cleanup"}]
    services jsonb NOT NULL DEFAULT '[]',
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX structures_corporation_idx ON structures (corporation_id);

-- The corporation each structure was last seen in (kept 30 days after it
-- goes): notifications are relayed only for structures of the corporation
-- the owner character read them for.
CREATE TABLE structure_owners (
    structure_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    seen_at timestamptz NOT NULL DEFAULT now()
);

-- Solar systems' security and region.
CREATE TABLE systems (
    system_id bigint PRIMARY KEY,
    name text NOT NULL,
    security_status double precision NOT NULL,
    region_id bigint NOT NULL
);

-- Names ESI gave: types, systems, regions, corporations, alliances,
-- attackers.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL
);

-- Structure notifications seen, by ESI's id: each is handled (turned into
-- a message and a timer) once. The same event seen by two owner
-- characters has two ids but one event key.
CREATE TABLE notifications (
    notification_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    type text NOT NULL,
    at timestamptz NOT NULL,
    structure_id bigint,
    event_key text NOT NULL UNIQUE,
    text text NOT NULL,
    handled boolean NOT NULL DEFAULT false
);
CREATE INDEX notifications_unhandled_idx ON notifications (at) WHERE NOT handled;

-- Reinforcement, anchoring and unanchoring timers, from notifications and
-- the structures' states.
CREATE TABLE timers (
    structure_id bigint NOT NULL,
    kind text NOT NULL,
    at timestamptz NOT NULL,
    corporation_id bigint NOT NULL,
    PRIMARY KEY (structure_id, kind, at)
);
CREATE INDEX timers_at_idx ON timers (at);

-- Tether's low-fuel alerts sent: one per structure and threshold, until
-- it's refuelled above it.
CREATE TABLE fuel_alerts (
    structure_id bigint NOT NULL,
    hours integer NOT NULL,
    PRIMARY KEY (structure_id, hours)
);

-- Discord messages to send, and sent. A key sends a message once.
CREATE TABLE outbox (
    id bigserial PRIMARY KEY,
    key text NOT NULL UNIQUE,
    channel text NOT NULL,
    message text NOT NULL,
    mention boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed text
);
CREATE INDEX outbox_pending_idx ON outbox (id) WHERE sent_at IS NULL AND failed IS NULL;
