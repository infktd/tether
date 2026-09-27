-- Moon surveys, places and ore prices: aa-moonmining's Moons, values and
-- Reports.

-- aa-moonmining's settings: the ore a drill pulls a day, the days in a
-- month (together a moon's monthly volume), and how long after its chunk
-- arrived an extraction moves to Past.
ALTER TABLE settings
    ADD COLUMN volume_per_day double precision NOT NULL DEFAULT 960400
        CHECK (volume_per_day > 0 AND volume_per_day <= 10000000),
    ADD COLUMN days_per_month double precision NOT NULL DEFAULT 30.4
        CHECK (days_per_month >= 28 AND days_per_month <= 31),
    ADD COLUMN stale_hours integer NOT NULL DEFAULT 12
        CHECK (stale_hours BETWEEN 1 AND 168);

-- An extraction the corporation no longer lists before its chunk
-- arrived: cancelled (or restarted, which ESI shows as a new one). Kept
-- for the Past tab instead of deleted.
ALTER TABLE extractions ADD COLUMN cancelled_at timestamptz;

-- Every moon Moon Mining knows: drilled by a refinery, or surveyed. The
-- system is ESI's once the moon is looked up (the survey's until then).
CREATE TABLE moons (
    moon_id bigint PRIMARY KEY,
    system_id bigint,
    -- When ESI confirmed the moon (its name is in `names`).
    checked_at timestamptz
);

-- Systems' security and place, from ESI.
CREATE TABLE systems (
    system_id bigint PRIMARY KEY,
    security double precision NOT NULL,
    constellation_id bigint NOT NULL,
    region_id bigint NOT NULL
);

-- The newest survey of each moon (an upload replaces the moon's last).
CREATE TABLE surveys (
    moon_id bigint PRIMARY KEY,
    -- Who uploaded it: their account (for My Uploaded Moons) and main.
    account_id bigint NOT NULL,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    uploaded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX surveys_account_idx ON surveys (account_id);

-- A survey's ores: each one's share of the moon (0 to 1).
CREATE TABLE survey_products (
    moon_id bigint NOT NULL REFERENCES surveys ON DELETE CASCADE,
    type_id bigint NOT NULL,
    amount double precision NOT NULL CHECK (amount > 0 AND amount <= 1),
    PRIMARY KEY (moon_id, type_id)
);

-- Moon ores and their rarity class (R4 to R64), from ESI's item groups.
CREATE TABLE ore_types (
    type_id bigint PRIMARY KEY,
    rarity integer NOT NULL CHECK (rarity IN (4, 8, 16, 32, 64))
);

-- CCP's prices (ESI's /markets/prices/) of the types Moon Mining values,
-- read daily.
CREATE TABLE prices (
    type_id bigint PRIMARY KEY,
    average_price double precision,
    adjusted_price double precision,
    updated_at timestamptz NOT NULL DEFAULT now()
);
