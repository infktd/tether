-- aa-freight's settings: its operation mode, the global price per volume
-- modifier (the contract handler's), the Discord channels for pilots and
-- customers, whether every contract is announced, the contract handler;
-- and how the last contract sync went.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    operation_mode text NOT NULL DEFAULT 'my_alliance'
        CHECK (operation_mode IN ('my_alliance', 'my_corporation', 'corp_in_alliance', 'corp_public')),
    price_per_volume_modifier double precision,
    pilot_channel text,
    customer_channel text,
    notify_all boolean NOT NULL DEFAULT false,
    -- The contract handler: which owner (a data-source character).
    handler_id bigint,
    synced_at timestamptz,
    sync_error text
);
INSERT INTO settings (id) VALUES (1);

-- Stations and structures routes start and end at.
CREATE TABLE locations (
    id bigint PRIMARY KEY,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    system_name text NOT NULL DEFAULT '',
    category text NOT NULL CHECK (category IN ('station', 'structure'))
);

-- aa-freight's pricings: a route and how its reward is worked out.
CREATE TABLE pricings (
    id serial PRIMARY KEY,
    start_location bigint NOT NULL REFERENCES locations (id),
    end_location bigint NOT NULL REFERENCES locations (id),
    active boolean NOT NULL DEFAULT true,
    bidirectional boolean NOT NULL DEFAULT true,
    price_base double precision,
    price_min double precision,
    price_per_volume double precision,
    use_modifier boolean NOT NULL DEFAULT false,
    price_per_collateral_percent double precision,
    collateral_min double precision,
    collateral_max double precision,
    volume_min double precision,
    volume_max double precision,
    days_to_expire integer,
    days_to_complete integer,
    details text NOT NULL DEFAULT '' CHECK (char_length(details) <= 2000),
    CHECK (start_location <> end_location)
);

-- The handler's courier contracts in scope of the operation mode.
CREATE TABLE contracts (
    contract_id bigint PRIMARY KEY,
    issuer_id bigint NOT NULL,
    issuer_corporation_id bigint NOT NULL,
    acceptor_id bigint,
    -- The pilot's corporation when they accepted it (from ESI's
    -- affiliation), for the statistics.
    acceptor_corporation_id bigint,
    start_location bigint NOT NULL,
    end_location bigint NOT NULL,
    status text NOT NULL,
    volume double precision NOT NULL DEFAULT 0,
    reward double precision NOT NULL DEFAULT 0,
    collateral double precision NOT NULL DEFAULT 0,
    days_to_complete integer,
    date_issued timestamptz NOT NULL,
    date_expired timestamptz,
    date_accepted timestamptz,
    date_completed timestamptz,
    title text NOT NULL DEFAULT '',
    notified_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX contracts_status_idx ON contracts (status);

-- Which statuses a contract's issuer was told about.
CREATE TABLE customer_notices (
    contract_id bigint NOT NULL REFERENCES contracts (contract_id) ON DELETE CASCADE,
    status text NOT NULL,
    PRIMARY KEY (contract_id, status)
);

-- Discord messages to send, each once.
CREATE TABLE outbox (
    id serial PRIMARY KEY,
    channel text NOT NULL,
    message text NOT NULL,
    queued_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed text
);

CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
