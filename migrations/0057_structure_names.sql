-- Structure names any member could read (Jay, 2026-10-05): when ESI won't
-- name a structure to an app's character (it names one only to a
-- character that may dock there), Tether asks through other members'
-- characters that granted esi-universe.read_structures.v1, and keeps
-- the name, its system and type for every app, a week. Nothing else read
-- with those tokens.
CREATE TABLE core.structure_names (
    structure_id bigint PRIMARY KEY,
    name text NOT NULL,
    solar_system_id bigint NOT NULL,
    type_id bigint,
    read_at timestamptz NOT NULL DEFAULT now()
);

-- Characters ESI wouldn't name a structure to, so they aren't asked
-- again for a week (each refusal spends ESI's error budget).
CREATE TABLE core.structure_name_misses (
    structure_id bigint NOT NULL,
    character_id bigint NOT NULL REFERENCES core.characters (id) ON DELETE CASCADE,
    at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (structure_id, character_id)
);
