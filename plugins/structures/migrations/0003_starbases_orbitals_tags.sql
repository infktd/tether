-- Starbases, customs offices and Orbital Skyhooks beside Upwell
-- structures; Metenox magmatic gas; fittings; tags; per-owner Discord
-- channels (aa-structures' webhooks per owner).

-- What a structure is. Upwell structures come from the structures read,
-- starbases from the starbases read, customs offices from theirs, and
-- skyhooks from the corporation's assets.
ALTER TABLE structures
    ADD COLUMN kind text NOT NULL DEFAULT 'upwell'
        CHECK (kind IN ('upwell', 'starbase', 'customs_office', 'skyhook')),
    -- A starbase's moon; a customs office's or skyhook's planet.
    ADD COLUMN moon_id bigint,
    ADD COLUMN planet_id bigint,
    -- A customs office's planet by name ("Customs Office (<planet>)").
    ADD COLUMN planet_name text,
    -- Where a skyhook is, to find its planet (the nearest).
    ADD COLUMN x double precision,
    ADD COLUMN y double precision,
    ADD COLUMN z double precision,
    -- Fuel by ESI's fuel_expires (Upwell) or fuel blocks (starbases); a
    -- Metenox's magmatic gas. fuel_expires is the earlier of the two.
    ADD COLUMN blocks_expires timestamptz,
    ADD COLUMN gas_expires timestamptz,
    ADD COLUMN fuel_blocks bigint,
    ADD COLUMN strontium bigint,
    ADD COLUMN magmatic_gas bigint,
    ADD COLUMN fuel_read_at timestamptz,
    -- Starbases: online since.
    ADD COLUMN onlined_since timestamptz,
    -- Customs offices: reinforcement window, access and tax rates, as ESI
    -- gives them.
    ADD COLUMN details jsonb NOT NULL DEFAULT '{}',
    -- From the assets: a quantum core in, anything fitted (unknown until
    -- the assets are read).
    ADD COLUMN has_core boolean,
    ADD COLUMN has_fitting boolean;
UPDATE structures SET blocks_expires = fuel_expires;
CREATE INDEX structures_moon_idx ON structures (moon_id) WHERE moon_id IS NOT NULL;
CREATE INDEX structures_planet_idx ON structures (planet_id) WHERE planet_id IS NOT NULL;

-- Each owner corporation's other reads, as for structures: last good
-- read, failures in a row, and no calls before (CCP requires Director).
ALTER TABLE owners
    ADD COLUMN starbases_at timestamptz,
    ADD COLUMN starbases_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN starbases_retry_at timestamptz,
    ADD COLUMN offices_at timestamptz,
    ADD COLUMN offices_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN offices_retry_at timestamptz,
    ADD COLUMN assets_at timestamptz,
    ADD COLUMN assets_failures integer NOT NULL DEFAULT 0,
    ADD COLUMN assets_retry_at timestamptz;

-- What sits in structures' slots and bays: fittings, fighters, fuel,
-- quantum cores, moon material. Replaced whole per corporation.
CREATE TABLE structure_items (
    item_id bigint PRIMARY KEY,
    structure_id bigint NOT NULL,
    corporation_id bigint NOT NULL,
    type_id bigint NOT NULL,
    flag text NOT NULL,
    quantity bigint NOT NULL
);
CREATE INDEX structure_items_structure_idx ON structure_items (structure_id);
CREATE INDEX structure_items_corporation_idx ON structure_items (corporation_id);

-- Planets near structures (customs offices, skyhooks): name and where.
CREATE TABLE planets (
    planet_id bigint PRIMARY KEY,
    system_id bigint NOT NULL,
    name text NOT NULL,
    x double precision NOT NULL,
    y double precision NOT NULL,
    z double precision NOT NULL
);
CREATE INDEX planets_system_idx ON planets (system_id);
-- A system's planets, once read.
ALTER TABLE systems ADD COLUMN planet_ids text;

-- Which alliance holds sovereignty in which system (for starbases' fuel
-- and the sov tag), and when it was read.
CREATE TABLE sovereignty (
    system_id bigint PRIMARY KEY,
    alliance_id bigint NOT NULL
);
ALTER TABLE settings
    ADD COLUMN sovereignty_at timestamptz,
    -- aa-structures' STRUCTURES_DEFAULT_TAGS_FILTER_ENABLED: the list
    -- shows structures with a default tag unless a filter is picked.
    ADD COLUMN default_tags_filter boolean NOT NULL DEFAULT false;

-- Notifications about starbases (by moon), customs offices and skyhooks
-- (by planet).
ALTER TABLE notifications
    ADD COLUMN moon_id bigint,
    ADD COLUMN planet_id bigint;

-- aa-structures' tags. Generated ones (space type, sov) are kept by the
-- sync; the rest managers make and assign. A default tag goes on every
-- new structure.
CREATE TABLE tags (
    id serial PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 40),
    description text NOT NULL DEFAULT '',
    style text NOT NULL DEFAULT 'default'
        CHECK (style IN ('default', 'primary', 'success', 'info', 'warning', 'danger')),
    sort_order integer NOT NULL DEFAULT 100,
    is_default boolean NOT NULL DEFAULT false,
    is_user_managed boolean NOT NULL DEFAULT true
);
CREATE TABLE structure_tags (
    structure_id bigint NOT NULL,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (structure_id, tag_id)
);
CREATE INDEX structure_tags_tag_idx ON structure_tags (tag_id);

-- Per owner: Discord channels per kind of notification instead of the
-- defaults (aa-structures' webhooks per owner), mentions, and whether its
-- customs offices are on the public list.
CREATE TABLE owner_settings (
    corporation_id bigint PRIMARY KEY,
    -- 'default' (the settings'), 'on' or 'off'.
    mention text NOT NULL DEFAULT 'default' CHECK (mention IN ('default', 'on', 'off')),
    pocos_public boolean NOT NULL DEFAULT false
);
-- A row per kind the owner routes itself: its channel, or none (not
-- sent). No row: the default channel.
CREATE TABLE owner_channels (
    corporation_id bigint NOT NULL,
    category text NOT NULL CHECK (category IN ('attack', 'fuel', 'state', 'moon')),
    channel text,
    PRIMARY KEY (corporation_id, category)
);

-- aa-structures' generated tags: space type and sovereignty, kept by the
-- sync.
INSERT INTO tags (name, description, style, sort_order, is_user_managed) VALUES
    ('highsec', 'In high security space', 'success', 100, false),
    ('lowsec', 'In low security space', 'warning', 100, false),
    ('nullsec', 'In null security space', 'danger', 100, false),
    ('w_space', 'In wormhole space', 'info', 100, false),
    ('sov', 'In a system the owner''s alliance holds sovereignty in', 'primary', 100, false);

-- Default tags go on structures first seen after this (not those known
-- already).
ALTER TABLE structures ADD COLUMN defaults_applied boolean NOT NULL DEFAULT false;
UPDATE structures SET defaults_applied = true;
