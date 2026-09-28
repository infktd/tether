-- aa-sov-timer's campaigns, as ESI last listed them, with the defender's
-- score the sync saw before (the page shows the trend to the live one).
CREATE TABLE campaigns (
    campaign_id bigint PRIMARY KEY,
    event_type text NOT NULL,
    system_id bigint NOT NULL,
    constellation_id bigint NOT NULL,
    defender_id bigint,
    start_time timestamptz NOT NULL,
    defender_score double precision,
    previous_score double precision
);

-- A constellation's region (/universe/names doesn't say).
CREATE TABLE constellations (
    constellation_id bigint PRIMARY KEY,
    region_id bigint NOT NULL
);

-- The campaigns' systems' activity defense multiplier.
CREATE TABLE adm (
    system_id bigint PRIMARY KEY,
    adm double precision NOT NULL
);

-- Names ESI gave: systems, constellations, regions, alliances.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
