-- Fleet Activity Tracking's own schema (the plugin's; the host runs this once).

-- Fleet types FCs pick from (aa-afat's), managed by manage_afat.
CREATE TABLE fleet_types (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    enabled boolean NOT NULL DEFAULT true
);
CREATE UNIQUE INDEX fleet_types_name ON fleet_types (lower(name));

-- FAT links. Members open `links/<hash>/add` to register attendance while
-- the link is open; the hash is random, so only people given the link
-- can use it.
CREATE TABLE links (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    hash text NOT NULL UNIQUE,
    fleet text NOT NULL,
    -- The fleet type's name when chosen: stats keep it if the type goes.
    fleet_type text,
    doctrine text,
    -- Who created it: their account and main at the time.
    creator_account bigint NOT NULL,
    creator_id bigint NOT NULL,
    creator_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    -- Times it was reopened after expiring.
    reopened integer NOT NULL DEFAULT 0
);
CREATE INDEX links_created ON links (created_at DESC);

-- One FAT per character per link. Corporation and alliance are the
-- character's when it was recorded, so stats don't move when pilots do.
CREATE TABLE fats (
    link_id bigint NOT NULL REFERENCES links ON DELETE CASCADE,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    corporation_id bigint,
    alliance_id bigint,
    -- Set for manual FATs: who added it.
    added_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (link_id, character_id)
);
CREATE INDEX fats_character ON fats (character_id);
CREATE INDEX fats_corporation ON fats (corporation_id);
CREATE INDEX fats_alliance ON fats (alliance_id);

-- Characters seen through the app (everyone who registered a FAT, with
-- all their characters), so managers can add a FAT by name.
CREATE TABLE characters (
    character_id bigint PRIMARY KEY,
    name text NOT NULL,
    corporation_id bigint NOT NULL,
    alliance_id bigint,
    seen_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX characters_name ON characters (lower(name));

-- Corporation and alliance names, from ESI.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL
);

-- aa-afat's logs: what FCs and managers did, kept LOG_DAYS days.
CREATE TABLE logs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now(),
    event text NOT NULL,
    actor_id bigint NOT NULL,
    actor_name text NOT NULL,
    link_hash text,
    description text NOT NULL
);
CREATE INDEX logs_at ON logs (at DESC);
