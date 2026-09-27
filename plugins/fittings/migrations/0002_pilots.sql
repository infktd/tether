-- "Can I fly it" (allianceauth-fittings' skill check): the skills of
-- characters registered for Fittings, read with its own
-- esi-skills.read_skills.v1. Only characters still registered are kept, and
-- only their pilot sees them.
CREATE TABLE character_skills (
    character_id bigint NOT NULL,
    skill_id bigint NOT NULL,
    -- ESI's active level: what the character may use now (an Alpha clone's
    -- may be lower than trained).
    level integer NOT NULL CHECK (level BETWEEN 0 AND 5),
    PRIMARY KEY (character_id, skill_id)
);

-- When each character's skills were last read, or why they couldn't be.
CREATE TABLE skill_reads (
    character_id bigint PRIMARY KEY,
    name text NOT NULL,
    read_at timestamptz,
    problem text
);
