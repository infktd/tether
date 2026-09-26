-- Member Audit's own schema (the plugin's; the host runs this once).

-- Every Member character registered with the plugin's scopes, with the
-- headline numbers of its last sync.
CREATE TABLE characters (
    character_id bigint PRIMARY KEY,
    name text NOT NULL,
    corporation_id bigint NOT NULL,
    alliance_id bigint,
    -- Last in the host's list of registered Member characters.
    seen_at timestamptz NOT NULL DEFAULT now(),
    synced_at timestamptz,
    total_sp bigint,
    unallocated_sp bigint,
    wallet double precision,
    system_id bigint,
    location_id bigint,
    ship_type_id bigint,
    ship_name text
);

CREATE TABLE skills (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    skill_id bigint NOT NULL,
    active_level integer NOT NULL,
    trained_level integer NOT NULL,
    sp bigint NOT NULL,
    PRIMARY KEY (character_id, skill_id)
);

CREATE TABLE queue (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    position integer NOT NULL,
    skill_id bigint NOT NULL,
    level integer NOT NULL,
    finish timestamptz,
    PRIMARY KEY (character_id, position)
);

CREATE TABLE assets (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    item_id bigint NOT NULL,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    location_id bigint NOT NULL,
    location_flag text NOT NULL,
    PRIMARY KEY (character_id, item_id)
);

CREATE TABLE journal (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    id bigint NOT NULL,
    at timestamptz NOT NULL,
    ref_type text NOT NULL,
    amount double precision,
    balance double precision,
    description text NOT NULL,
    PRIMARY KEY (character_id, id)
);

CREATE TABLE clones (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    jump_clone_id bigint NOT NULL,
    location_id bigint NOT NULL,
    implants jsonb NOT NULL DEFAULT '[]',
    PRIMARY KEY (character_id, jump_clone_id)
);

CREATE TABLE implants (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    type_id bigint NOT NULL,
    PRIMARY KEY (character_id, type_id)
);

-- Names ESI gave: skills, items, systems, stations, corporations.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL
);
CREATE INDEX names_lower_idx ON names (lower(name));

-- AA's Skill Sets: named lists of skills at a level, e.g. a doctrine.
CREATE TABLE skill_sets (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 100)
);
CREATE TABLE skill_set_skills (
    set_id bigint NOT NULL REFERENCES skill_sets ON DELETE CASCADE,
    skill_id bigint NOT NULL,
    level integer NOT NULL CHECK (level BETWEEN 1 AND 5),
    PRIMARY KEY (set_id, skill_id)
);
