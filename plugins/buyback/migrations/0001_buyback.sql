-- Buyback: aa-buybackprogram 1:1 (Jay, 2026-10-07). A program's manager
-- is one of the app's data sources (a character and its corporation),
-- which stands for aa-buybackprogram's Owner. Accounts are Tether's
-- account ids (`identity.viewer().account_id`), never names.

-- aa-buybackprogram's Django settings, editable here, with its defaults.
CREATE TABLE settings (
    id integer PRIMARY KEY CHECK (id = 1),
    -- BUYBACKPROGRAM_PRICE_METHOD: 'Fuzzwork' or 'Janice'.
    price_method text NOT NULL DEFAULT 'Fuzzwork' CHECK (price_method IN ('Fuzzwork', 'Janice')),
    -- BUYBACKPROGRAM_PRICE_SOURCE_ID / _NAME: Jita IV - Moon 4 - CNAP.
    price_source_id bigint NOT NULL DEFAULT 60003760,
    price_source_name text NOT NULL DEFAULT 'Jita',
    -- BUYBACKPROGRAM_PRICE_INSTANT_PRICES: max buy / min sell, not the
    -- top 5% average.
    instant_prices boolean NOT NULL DEFAULT false,
    price_age_warning_hours integer NOT NULL DEFAULT 48,
    -- BUYBACKPROGRAM_UNUSED_TRACKING_PURGE_LIMIT (0: never).
    purge_hours integer NOT NULL DEFAULT 48,
    track_prefill_contracts boolean NOT NULL DEFAULT true,
    tracking_prefill text NOT NULL DEFAULT 'aa-bbp',
    show_location_count integer NOT NULL DEFAULT 4,
    disallow_any_disallowed boolean NOT NULL DEFAULT false,
    reverse_enabled boolean NOT NULL DEFAULT true,
    restrict_tracking_details boolean NOT NULL DEFAULT false,
    prices_updated_at timestamptz,
    sync_error text
);
INSERT INTO settings (id) VALUES (1);

-- Where contracts are accepted: a station or structure, or a free name.
CREATE TABLE locations (
    id bigserial PRIMARY KEY,
    owner_character bigint NOT NULL,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 32),
    system_id bigint,
    structure_id bigint,
    created_by bigint NOT NULL,
    UNIQUE (owner_character, name, system_id, structure_id)
);

CREATE TABLE programs (
    id bigserial PRIMARY KEY,
    name text NOT NULL DEFAULT '' CHECK (length(name) <= 64),
    tracking_prefill text NOT NULL DEFAULT '' CHECK (tracking_prefill ~ '^[A-Za-z0-9._-]{0,16}$'),
    owner_character bigint NOT NULL,
    owner_corporation bigint NOT NULL,
    -- The account that set it up with its own character (AA's
    -- owner.user): who hears of its contracts.
    manager_account bigint NOT NULL,
    is_corporation boolean NOT NULL DEFAULT false,
    expiration text NOT NULL DEFAULT '2 Weeks'
        CHECK (expiration IN ('1 Day', '3 Days', '1 Week', '2 Weeks', '4 Weeks')),
    price_type text NOT NULL DEFAULT 'Buy' CHECK (price_type IN ('Buy', 'Sell', 'Split')),
    tax integer NOT NULL DEFAULT 0 CHECK (tax BETWEEN 0 AND 100),
    hauling_fuel_cost integer NOT NULL DEFAULT 0,
    density_modifier boolean NOT NULL DEFAULT false,
    compression_density_modifier boolean NOT NULL DEFAULT false,
    density_threshold integer NOT NULL DEFAULT 0,
    density_tax integer NOT NULL DEFAULT 0 CHECK (density_tax BETWEEN 0 AND 100),
    allow_all_items boolean NOT NULL DEFAULT true,
    use_refined_value boolean NOT NULL DEFAULT false,
    use_compressed_value boolean NOT NULL DEFAULT false,
    use_raw_ore_value boolean NOT NULL DEFAULT true,
    allow_unpacked_items boolean NOT NULL DEFAULT false,
    refining_rate numeric(5, 2) NOT NULL DEFAULT 0 CHECK (refining_rate BETWEEN 0 AND 100),
    use_t1_scrap boolean NOT NULL DEFAULT false,
    t1_refining_rate numeric(5, 2) NOT NULL DEFAULT 50 CHECK (t1_refining_rate BETWEEN 0 AND 100),
    blue_loot_npc_price boolean NOT NULL DEFAULT false,
    red_loot_npc_price boolean NOT NULL DEFAULT false,
    ope_npc_price boolean NOT NULL DEFAULT false,
    bonds_npc_price boolean NOT NULL DEFAULT false,
    -- Group ids (identity groups) and state names; empty: anyone.
    restricted_groups bigint[] NOT NULL DEFAULT '{}',
    restricted_states text[] NOT NULL DEFAULT '{}',
    -- Every pilot who can log in, whatever their groups and state.
    is_public boolean NOT NULL DEFAULT false,
    -- aa-buybackprogram's discord_dm_notification: the manager hears of
    -- new contracts in Tether's notifications.
    notify_manager boolean NOT NULL DEFAULT false,
    discord_show_item_list boolean NOT NULL DEFAULT false,
    -- aa-buybackprogram's webhook: one of the app's Discord channels.
    discord_channel text,
    -- The funding wallet: a wallet division of the owner's corporation.
    wallet_division integer,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE program_locations (
    program_id bigint NOT NULL REFERENCES programs ON DELETE CASCADE,
    location_id bigint NOT NULL REFERENCES locations ON DELETE CASCADE,
    PRIMARY KEY (program_id, location_id)
);

-- Special taxes and the allow list, by type or market group.
CREATE TABLE program_items (
    id bigserial PRIMARY KEY,
    program_id bigint NOT NULL REFERENCES programs ON DELETE CASCADE,
    type_id bigint,
    market_group_id bigint,
    item_tax integer NOT NULL DEFAULT 0 CHECK (item_tax BETWEEN -100 AND 100),
    disallow_item boolean NOT NULL DEFAULT false,
    CHECK ((type_id IS NULL) <> (market_group_id IS NULL))
);
CREATE UNIQUE INDEX program_items_type ON program_items (program_id, type_id) WHERE type_id IS NOT NULL;
CREATE UNIQUE INDEX program_items_market_group ON program_items (program_id, market_group_id)
    WHERE market_group_id IS NOT NULL;

CREATE TABLE static_prices (
    program_id bigint NOT NULL REFERENCES programs ON DELETE CASCADE,
    type_id bigint NOT NULL,
    price numeric(20, 2) NOT NULL CHECK (price >= 0),
    PRIMARY KEY (program_id, type_id)
);

-- The manual-review watchlist: a type or an inventory group.
CREATE TABLE watchlist (
    id bigserial PRIMARY KEY,
    program_id bigint NOT NULL REFERENCES programs ON DELETE CASCADE,
    type_id bigint,
    group_id bigint,
    CHECK ((type_id IS NULL) <> (group_id IS NULL))
);
CREATE UNIQUE INDEX watchlist_type ON watchlist (program_id, type_id) WHERE type_id IS NOT NULL;
CREATE UNIQUE INDEX watchlist_group ON watchlist (program_id, group_id) WHERE group_id IS NOT NULL;

-- Market prices (Fuzzwork or Janice), one row a type, for every program.
CREATE TABLE item_prices (
    type_id bigint PRIMARY KEY,
    buy numeric(20, 2) NOT NULL DEFAULT 0,
    sell numeric(20, 2) NOT NULL DEFAULT 0,
    updated timestamptz NOT NULL DEFAULT now()
);

-- ESI's average prices (/markets/prices), the "NPC" price of loot.
CREATE TABLE npc_prices (
    type_id bigint PRIMARY KEY,
    average numeric(20, 2) NOT NULL,
    updated timestamptz NOT NULL DEFAULT now()
);

-- Contracts read from ESI: normal and reverse ones alike.
CREATE TABLE contracts (
    contract_id bigint PRIMARY KEY,
    assignee_id bigint NOT NULL,
    availability text NOT NULL DEFAULT '',
    date_completed timestamptz,
    date_expired timestamptz,
    date_issued timestamptz NOT NULL,
    for_corporation boolean NOT NULL DEFAULT false,
    issuer_corporation_id bigint NOT NULL,
    issuer_id bigint NOT NULL,
    start_location_id bigint,
    location_name text,
    price numeric(20, 2) NOT NULL DEFAULT 0,
    status text NOT NULL,
    title text NOT NULL DEFAULT '',
    volume numeric(20, 2) NOT NULL DEFAULT 0,
    -- Has a buyback prefix but no tracking: a possible scam.
    no_tracking boolean NOT NULL DEFAULT false,
    is_reverse boolean NOT NULL DEFAULT false,
    -- The manager it was read through, and whether from its
    -- corporation's contracts.
    owner_character bigint NOT NULL,
    from_corporation boolean NOT NULL DEFAULT false,
    items_read boolean NOT NULL DEFAULT false,
    seen_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX contracts_assignee ON contracts (assignee_id);
CREATE INDEX contracts_issuer ON contracts (issuer_id);

CREATE TABLE contract_items (
    contract_id bigint NOT NULL REFERENCES contracts ON DELETE CASCADE,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL
);
CREATE INDEX contract_items_contract ON contract_items (contract_id);

-- A contract's flags (aa-buybackprogram's ContractNotification).
CREATE TABLE contract_flags (
    id bigserial PRIMARY KEY,
    contract_id bigint NOT NULL REFERENCES contracts ON DELETE CASCADE,
    tone text NOT NULL CHECK (tone IN ('danger', 'warning', 'success', 'info', 'watch')),
    header text NOT NULL,
    message text NOT NULL
);
CREATE INDEX contract_flags_contract ON contract_flags (contract_id);

-- A calculation and its tracking number.
CREATE TABLE trackings (
    id bigserial PRIMARY KEY,
    program_id bigint REFERENCES programs ON DELETE SET NULL,
    contract_id bigint REFERENCES contracts ON DELETE SET NULL,
    issuer_account bigint,
    issuer_character bigint,
    value numeric(20, 2) NOT NULL DEFAULT 0,
    taxes numeric(20, 2) NOT NULL DEFAULT 0,
    hauling_cost numeric(20, 2) NOT NULL DEFAULT 0,
    donation numeric(20, 2) NOT NULL DEFAULT 0,
    net_price numeric(20, 2) NOT NULL DEFAULT 0,
    total_volume double precision NOT NULL DEFAULT 0,
    tracking_number text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    notes text
);
CREATE INDEX trackings_contract ON trackings (contract_id);
CREATE INDEX trackings_issuer ON trackings (issuer_account);

CREATE TABLE tracking_items (
    tracking_id bigint NOT NULL REFERENCES trackings ON DELETE CASCADE,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    -- Per unit, after taxes.
    buy_value numeric(20, 2) NOT NULL DEFAULT 0
);
CREATE INDEX tracking_items_tracking ON tracking_items (tracking_id);

-- Per pilot: sellers' accepted and rejected notices off.
CREATE TABLE user_settings (
    account_id bigint PRIMARY KEY,
    disable_notifications boolean NOT NULL DEFAULT false
);

CREATE TABLE faq (
    id bigserial PRIMARY KEY,
    header text NOT NULL CHECK (length(header) BETWEEN 1 AND 1024),
    body text NOT NULL CHECK (length(body) <= 10000),
    position integer NOT NULL DEFAULT 0
);

CREATE TABLE structure_names (
    structure_id bigint PRIMARY KEY,
    name text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- The owner corporations' wallet and hangar divisions.
CREATE TABLE wallets (
    corporation_id bigint NOT NULL,
    division integer NOT NULL,
    name text NOT NULL,
    balance numeric(20, 2) NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (corporation_id, division)
);

CREATE TABLE hangar_divisions (
    corporation_id bigint NOT NULL,
    division integer NOT NULL,
    name text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (corporation_id, division)
);

-- Assembled containers in the owner corporations' hangars.
CREATE TABLE containers (
    item_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    name text NOT NULL,
    type_id bigint NOT NULL,
    structure_id bigint,
    location_flag text NOT NULL DEFAULT '',
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Reverse buyback: selling hangar stock to members.
CREATE TABLE reverse_programs (
    id bigserial PRIMARY KEY,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    tracking_prefill text NOT NULL DEFAULT '' CHECK (tracking_prefill ~ '^[A-Za-z0-9._-]{0,16}$'),
    owner_character bigint NOT NULL,
    owner_corporation bigint NOT NULL,
    -- The account that set it up with its own character (AA's
    -- owner.user): who hears of its contracts.
    manager_account bigint NOT NULL,
    is_corporation boolean NOT NULL DEFAULT false,
    stock_source text NOT NULL DEFAULT 'division' CHECK (stock_source IN ('division', 'containers')),
    hangar_division integer,
    expiration text NOT NULL DEFAULT '2 Weeks'
        CHECK (expiration IN ('1 Day', '3 Days', '1 Week', '2 Weeks', '4 Weeks')),
    price_type text NOT NULL DEFAULT 'Sell' CHECK (price_type IN ('Buy', 'Sell', 'Split')),
    markup integer NOT NULL DEFAULT 0 CHECK (markup BETWEEN -100 AND 100),
    restricted_groups bigint[] NOT NULL DEFAULT '{}',
    restricted_states text[] NOT NULL DEFAULT '{}',
    is_public boolean NOT NULL DEFAULT false,
    notify_manager boolean NOT NULL DEFAULT false,
    discord_channel text,
    stock_synced_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE reverse_program_locations (
    program_id bigint NOT NULL REFERENCES reverse_programs ON DELETE CASCADE,
    location_id bigint NOT NULL REFERENCES locations ON DELETE CASCADE,
    PRIMARY KEY (program_id, location_id)
);

CREATE TABLE reverse_program_containers (
    program_id bigint NOT NULL REFERENCES reverse_programs ON DELETE CASCADE,
    item_id bigint NOT NULL,
    PRIMARY KEY (program_id, item_id)
);

CREATE TABLE hangar_stock (
    program_id bigint NOT NULL REFERENCES reverse_programs ON DELETE CASCADE,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (program_id, type_id)
);

CREATE TABLE reverse_trackings (
    id bigserial PRIMARY KEY,
    program_id bigint REFERENCES reverse_programs ON DELETE SET NULL,
    contract_id bigint REFERENCES contracts ON DELETE SET NULL,
    issuer_account bigint,
    issuer_character bigint,
    net_price numeric(20, 2) NOT NULL DEFAULT 0,
    tracking_number text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    notes text
);
CREATE INDEX reverse_trackings_contract ON reverse_trackings (contract_id);

CREATE TABLE reverse_tracking_items (
    tracking_id bigint NOT NULL REFERENCES reverse_trackings ON DELETE CASCADE,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    buy_value numeric(20, 2) NOT NULL DEFAULT 0
);
CREATE INDEX reverse_tracking_items_tracking ON reverse_tracking_items (tracking_id);

-- A pilot's cart in a reverse program, before they check its price.
CREATE TABLE reverse_carts (
    account_id bigint NOT NULL,
    program_id bigint NOT NULL REFERENCES reverse_programs ON DELETE CASCADE,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (account_id, program_id, type_id)
);

-- Discord cards waiting for the relay.
CREATE TABLE outbox (
    id bigserial PRIMARY KEY,
    channel text NOT NULL,
    card jsonb NOT NULL,
    page text,
    queued_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed text
);
