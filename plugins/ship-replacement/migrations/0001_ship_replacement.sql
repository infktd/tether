-- Ship Replacement's own schema (the plugin's; the host runs this once).
-- AA's srp: SRP fleets, pilots' requests for their losses on them, and
-- reviewers' comments; plus zKillboard's answers, kept once fetched.

CREATE TABLE fleets (
    id bigserial PRIMARY KEY,
    name text NOT NULL,
    doctrine text NOT NULL DEFAULT '',
    fleet_commander text NOT NULL,
    fleet_time timestamptz NOT NULL,
    -- After action report, as written.
    aar text NOT NULL DEFAULT '',
    -- The SRP code pilots request with (AA's fleet_srp_code).
    srp_code text NOT NULL UNIQUE,
    -- AA's "Completed": no more requests.
    completed boolean NOT NULL DEFAULT false,
    created_by_account_id bigint NOT NULL,
    created_by_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX fleets_time_idx ON fleets (fleet_time DESC);

CREATE TABLE requests (
    id bigserial PRIMARY KEY,
    fleet_id bigint NOT NULL REFERENCES fleets ON DELETE CASCADE,
    account_id bigint NOT NULL,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    -- One request per loss, ever.
    killmail_id bigint NOT NULL UNIQUE,
    killmail_hash text NOT NULL,
    killboard_link text NOT NULL,
    ship_type_id bigint NOT NULL,
    ship_name text NOT NULL,
    solar_system_id bigint NOT NULL DEFAULT 0,
    killmail_time timestamptz NOT NULL,
    -- zKillboard's totalValue when requested (AA's kb_total_loss); null
    -- if zKillboard couldn't say.
    kb_total_loss double precision,
    -- What will be paid (AA's srp_total_amount): set when approved, or by
    -- a reviewer; null until then.
    payout double precision CHECK (payout IS NULL OR payout >= 0),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    paid boolean NOT NULL DEFAULT false,
    additional_info text NOT NULL DEFAULT '',
    reviewer_name text,
    decided_at timestamptz,
    paid_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (NOT paid OR status = 'approved')
);
CREATE INDEX requests_fleet_idx ON requests (fleet_id, created_at);
CREATE INDEX requests_account_idx ON requests (account_id, created_at DESC);

CREATE TABLE comments (
    id bigserial PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES requests ON DELETE CASCADE,
    author_account_id bigint NOT NULL,
    author_name text NOT NULL,
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX comments_request_idx ON comments (request_id, created_at);

-- Every loss ever requested. Kept when a fleet (and its requests) is
-- removed, so a loss can't be requested, and paid, twice.
CREATE TABLE claimed_kills (
    killmail_id bigint PRIMARY KEY,
    claimed_at timestamptz NOT NULL DEFAULT now()
);

-- Kills zKillboard had no answer for: not asked again for 10 minutes.
CREATE TABLE zkb_misses (
    killmail_id bigint PRIMARY KEY,
    fetched_at timestamptz NOT NULL DEFAULT now()
);

-- Links that didn't check out, per account: a few are allowed, then the
-- pilot waits, so nobody spends the app's shared ESI error allowance or
-- zKillboard quota. Kept a day.
CREATE TABLE failed_lookups (
    id bigserial PRIMARY KEY,
    account_id bigint NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX failed_lookups_account_idx ON failed_lookups (account_id, at);

-- zKillboard's hash and value for a kill, once fetched: asked again only
-- if it had no answer.
CREATE TABLE zkb_values (
    killmail_id bigint PRIMARY KEY,
    killmail_hash text NOT NULL,
    total_value double precision NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now()
);
