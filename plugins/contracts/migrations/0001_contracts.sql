-- Where notices go, which are sent, how close a price must be to its
-- appraisal, and how the last sync went.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    channel text,
    notify_new boolean NOT NULL DEFAULT true,
    notify_completed boolean NOT NULL DEFAULT true,
    notify_ended boolean NOT NULL DEFAULT false,
    tolerance_percent double precision NOT NULL DEFAULT 1
        CHECK (tolerance_percent BETWEEN 0 AND 100),
    synced_at timestamptz,
    sync_error text
);
INSERT INTO settings (id) VALUES (1);

-- Contracts assigned to an owner's corporation, as ESI last said.
CREATE TABLE contracts (
    contract_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    type text NOT NULL,
    status text NOT NULL,
    issuer_id bigint NOT NULL,
    issuer_corporation_id bigint NOT NULL,
    acceptor_id bigint,
    start_location bigint,
    end_location bigint,
    price double precision NOT NULL DEFAULT 0,
    reward double precision NOT NULL DEFAULT 0,
    collateral double precision NOT NULL DEFAULT 0,
    volume double precision NOT NULL DEFAULT 0,
    title text NOT NULL DEFAULT '',
    date_issued timestamptz NOT NULL,
    date_expired timestamptz,
    date_completed timestamptz,
    -- [[type_id, quantity, included], ...] once read; included is what the
    -- issuer hands over, the rest what they ask for.
    items jsonb,
    items_error text,
    -- The Janice appraisal the description links, and its buy total once
    -- read (or why not).
    appraisal_code text,
    appraisal_buy double precision,
    -- Why a read appraisal doesn't vouch for the contract (other items,
    -- too old), or why it wasn't read.
    appraisal_problem text,
    appraisal_note text,
    checked_at timestamptz,
    -- Found by its corporation's first read: never announced as new.
    backlog boolean NOT NULL DEFAULT false,
    first_seen timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX contracts_issued_idx ON contracts (date_issued DESC);

-- Corporations whose contracts have been read: what the first read finds
-- is their backlog, never announced as new.
CREATE TABLE corporations (
    corporation_id bigint PRIMARY KEY,
    first_read_at timestamptz NOT NULL DEFAULT now()
);

-- Names ESI gave: characters, corporations, item types.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);

-- Stations and structures by id, with their system.
CREATE TABLE locations (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    system_name text NOT NULL DEFAULT ''
);

-- Each contract's notices, sent once: new, completed, ended.
CREATE TABLE notices (
    contract_id bigint NOT NULL,
    event text NOT NULL CHECK (event IN ('new', 'completed', 'ended')),
    PRIMARY KEY (contract_id, event)
);

-- Discord cards to send, and sent.
CREATE TABLE outbox (
    id serial PRIMARY KEY,
    channel text NOT NULL,
    message text NOT NULL,
    card jsonb,
    queued_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed text
);
