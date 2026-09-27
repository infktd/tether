-- The full character sheet (aa-memberaudit's tabs): what each section
-- stores, and when each was last read for each character.

-- When each section was last read (or tried) for a character, and why it
-- failed if it did: the sync picks the most overdue first, and each tab
-- says how fresh it is.
CREATE TABLE section_syncs (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    section text NOT NULL,
    synced_at timestamptz NOT NULL,
    ok boolean NOT NULL DEFAULT true,
    error text,
    PRIMARY KEY (character_id, section)
);

-- The public sheet, clones' home and jump dates.
ALTER TABLE characters ADD COLUMN birthday timestamptz;
ALTER TABLE characters ADD COLUMN security_status double precision;
ALTER TABLE characters ADD COLUMN faction_id bigint;
ALTER TABLE characters ADD COLUMN bio text;
ALTER TABLE characters ADD COLUMN home_location_id bigint;
ALTER TABLE characters ADD COLUMN last_clone_jump timestamptz;
ALTER TABLE characters ADD COLUMN last_station_change timestamptz;
ALTER TABLE characters ADD COLUMN location_type text;

-- The skill in training's span, for a live progress bar.
ALTER TABLE queue ADD COLUMN start timestamptz;
ALTER TABLE queue ADD COLUMN level_start_sp bigint;
ALTER TABLE queue ADD COLUMN level_end_sp bigint;
ALTER TABLE queue ADD COLUMN training_start_sp bigint;

-- The journal's two parties (AA's First party and Second party).
ALTER TABLE journal ADD COLUMN first_party_id bigint;
ALTER TABLE journal ADD COLUMN second_party_id bigint;

-- Whether an asset sits in another item (a container, a ship).
ALTER TABLE assets ADD COLUMN location_type text;

CREATE TABLE transactions (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    id bigint NOT NULL,
    at timestamptz NOT NULL,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    unit_price double precision NOT NULL,
    client_id bigint NOT NULL,
    location_id bigint NOT NULL,
    is_buy boolean NOT NULL,
    is_personal boolean NOT NULL,
    PRIMARY KEY (character_id, id)
);

CREATE TABLE contracts (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    contract_id bigint NOT NULL,
    kind text NOT NULL,
    status text NOT NULL,
    availability text NOT NULL,
    issuer_id bigint NOT NULL,
    assignee_id bigint NOT NULL,
    acceptor_id bigint NOT NULL,
    issued timestamptz NOT NULL,
    expires timestamptz NOT NULL,
    completed timestamptz,
    title text NOT NULL DEFAULT '',
    price double precision,
    reward double precision,
    collateral double precision,
    volume double precision,
    start_location_id bigint,
    end_location_id bigint,
    -- Its items were read (only item exchanges and auctions have them).
    items_read boolean NOT NULL DEFAULT false,
    PRIMARY KEY (character_id, contract_id)
);

CREATE TABLE contract_items (
    character_id bigint NOT NULL,
    contract_id bigint NOT NULL,
    record_id bigint NOT NULL,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    is_included boolean NOT NULL,
    PRIMARY KEY (character_id, contract_id, record_id),
    FOREIGN KEY (character_id, contract_id) REFERENCES contracts ON DELETE CASCADE
);

CREATE TABLE contacts (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    contact_id bigint NOT NULL,
    contact_type text NOT NULL,
    standing double precision NOT NULL,
    is_watched boolean NOT NULL DEFAULT false,
    is_blocked boolean NOT NULL DEFAULT false,
    PRIMARY KEY (character_id, contact_id)
);

-- NPC standings: agents, NPC corporations and factions.
CREATE TABLE standings (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    from_id bigint NOT NULL,
    from_type text NOT NULL,
    standing double precision NOT NULL,
    PRIMARY KEY (character_id, from_id)
);

-- Mail: headers from ESI's newest, each body read once and kept.
CREATE TABLE mails (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    mail_id bigint NOT NULL,
    at timestamptz NOT NULL,
    from_id bigint NOT NULL,
    subject text NOT NULL DEFAULT '',
    is_read boolean NOT NULL DEFAULT false,
    labels jsonb NOT NULL DEFAULT '[]',
    recipients jsonb NOT NULL DEFAULT '[]',
    body text,
    PRIMARY KEY (character_id, mail_id)
);
CREATE INDEX mails_unread_idx ON mails (character_id) WHERE body IS NULL;

CREATE TABLE mail_labels (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    label_id bigint NOT NULL,
    name text NOT NULL,
    unread bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (character_id, label_id)
);

CREATE TABLE mailing_lists (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    mailing_list_id bigint NOT NULL,
    name text NOT NULL,
    PRIMARY KEY (character_id, mailing_list_id)
);

CREATE TABLE loyalty (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    corporation_id bigint NOT NULL,
    points bigint NOT NULL,
    PRIMARY KEY (character_id, corporation_id)
);

CREATE TABLE planets (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    planet_id bigint NOT NULL,
    solar_system_id bigint NOT NULL,
    planet_type text NOT NULL,
    upgrade_level integer NOT NULL,
    pins integer NOT NULL,
    last_update timestamptz NOT NULL,
    PRIMARY KEY (character_id, planet_id)
);

CREATE TABLE industry_jobs (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    job_id bigint NOT NULL,
    activity_id integer NOT NULL,
    blueprint_type_id bigint NOT NULL,
    product_type_id bigint,
    runs integer NOT NULL,
    status text NOT NULL,
    facility_id bigint NOT NULL,
    starts timestamptz NOT NULL,
    ends timestamptz NOT NULL,
    cost double precision,
    PRIMARY KEY (character_id, job_id)
);

CREATE TABLE blueprints (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    item_id bigint NOT NULL,
    type_id bigint NOT NULL,
    location_id bigint NOT NULL,
    location_flag text NOT NULL,
    quantity integer NOT NULL,
    runs integer NOT NULL,
    material_efficiency integer NOT NULL,
    time_efficiency integer NOT NULL,
    PRIMARY KEY (character_id, item_id)
);

CREATE TABLE orders (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    order_id bigint NOT NULL,
    type_id bigint NOT NULL,
    is_buy boolean NOT NULL,
    price double precision NOT NULL,
    volume_total bigint NOT NULL,
    volume_remain bigint NOT NULL,
    location_id bigint NOT NULL,
    issued timestamptz NOT NULL,
    duration integer NOT NULL,
    is_corporation boolean NOT NULL,
    PRIMARY KEY (character_id, order_id)
);

-- Kills and losses: ids and hashes from the character, the rest from the
-- public killmail.
CREATE TABLE killmails (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    killmail_id bigint NOT NULL,
    hash text NOT NULL,
    at timestamptz,
    solar_system_id bigint,
    victim_id bigint,
    victim_corporation_id bigint,
    victim_alliance_id bigint,
    ship_type_id bigint,
    attackers integer,
    PRIMARY KEY (character_id, killmail_id)
);

CREATE TABLE corporation_history (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    record_id bigint NOT NULL,
    corporation_id bigint NOT NULL,
    start_date timestamptz NOT NULL,
    is_deleted boolean NOT NULL DEFAULT false,
    PRIMARY KEY (character_id, record_id)
);

CREATE TABLE attributes (
    character_id bigint PRIMARY KEY REFERENCES characters ON DELETE CASCADE,
    charisma integer NOT NULL,
    intelligence integer NOT NULL,
    memory integer NOT NULL,
    perception integer NOT NULL,
    willpower integer NOT NULL,
    bonus_remaps integer,
    last_remap timestamptz,
    remap_cooldown timestamptz
);

-- Corporation roles, by where they apply (`roles`, `roles_at_hq`,
-- `roles_at_base`, `roles_at_other`).
CREATE TABLE roles (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    scope text NOT NULL,
    role text NOT NULL,
    PRIMARY KEY (character_id, scope, role)
);

CREATE TABLE titles (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    title_id bigint NOT NULL,
    name text NOT NULL,
    PRIMARY KEY (character_id, title_id)
);

CREATE TABLE mining (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    day date NOT NULL,
    type_id bigint NOT NULL,
    solar_system_id bigint NOT NULL,
    quantity bigint NOT NULL,
    PRIMARY KEY (character_id, day, type_id, solar_system_id)
);

-- Which group each skill is in (public: the skills category's groups).
CREATE TABLE skill_groups (
    group_id bigint PRIMARY KEY,
    name text NOT NULL,
    read_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE skill_types (
    type_id bigint PRIMARY KEY,
    group_id bigint NOT NULL REFERENCES skill_groups ON DELETE CASCADE
);

-- Places `names` can't name (structures a character may dock at, and
-- planets), and when naming one last failed, so it isn't asked again
-- every run.
CREATE TABLE unnamed (
    id bigint PRIMARY KEY,
    tried_at timestamptz NOT NULL
);

-- "Update now": one character's sections, all of them, as soon as a job
-- runs; asked for at most every 10 minutes per character, and when it
-- was done.
ALTER TABLE characters ADD COLUMN update_requested_at timestamptz;
ALTER TABLE characters ADD COLUMN update_done_at timestamptz;

-- Who asked for updates of other people's characters, and when: a few an
-- hour each, so nobody can spend the app's ESI budget in bulk.
CREATE TABLE update_asks (
    account_id bigint NOT NULL,
    character_id bigint NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX update_asks_account_idx ON update_asks (account_id, at);
