-- Moon Mining's own schema (the plugin's; the host runs this once).

-- One row of settings.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    -- Hours a popped moon stays Members-only before Blue see it.
    fresh_hours integer NOT NULL DEFAULT 4 CHECK (fresh_hours BETWEEN 1 AND 48),
    -- The Discord channel pops are posted to (an assigned one), if any.
    ping_channel text,
    pings boolean NOT NULL DEFAULT true
);
INSERT INTO settings DEFAULT VALUES;

-- Refineries, from the data sources' corporation structures.
CREATE TABLE structures (
    structure_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    name text NOT NULL,
    system_id bigint,
    type_id bigint,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Every extraction seen, kept after it pops (ESI forgets them).
CREATE TABLE extractions (
    structure_id bigint NOT NULL,
    chunk_arrival timestamptz NOT NULL,
    moon_id bigint NOT NULL,
    corporation_id bigint NOT NULL,
    extraction_start timestamptz NOT NULL,
    -- The automatic fracture: when the moon pops unless fired earlier.
    natural_decay timestamptz NOT NULL,
    seen_at timestamptz NOT NULL DEFAULT now(),
    -- The pop its queued ping is for: queued again only when this moves.
    queued_for timestamptz,
    pinged boolean NOT NULL DEFAULT false,
    PRIMARY KEY (structure_id, chunk_arrival)
);
CREATE INDEX extractions_decay_idx ON extractions (natural_decay);

-- Names ESI gave: moons, systems, characters, ore types.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL
);

-- Mining observers, and when each ledger was last read: a run reads the
-- oldest few, so every observer gets its turn however many there are.
CREATE TABLE observers (
    observer_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    synced_at timestamptz
);

-- Mining observers' ledgers: who mined what, per day.
CREATE TABLE ledger (
    observer_id bigint NOT NULL,
    character_id bigint NOT NULL,
    type_id bigint NOT NULL,
    day date NOT NULL,
    corporation_id bigint NOT NULL,
    quantity bigint NOT NULL,
    PRIMARY KEY (observer_id, character_id, type_id, day)
);
CREATE INDEX ledger_day_idx ON ledger (day);

-- Characters with the in-game Station Manager role, from the data
-- sources' corporation roles.
CREATE TABLE station_managers (
    character_id bigint PRIMARY KEY,
    corporation_id bigint NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Each corporation's pop cadence, set by its Station Managers.
CREATE TABLE cadences (
    corporation_id bigint PRIMARY KEY,
    every_hours integer NOT NULL CHECK (every_hours BETWEEN 1 AND 168),
    at_time text NOT NULL CHECK (at_time ~ '^[0-2][0-9]:[0-5][0-9]$')
);
