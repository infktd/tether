-- The skills each item type requires (its dogma attributes requiredSkill1
-- to 6 and their levels, from ESI's public universe-type), as
-- [[skill, level]]: read once per type for skill sets made from a fitting.
CREATE TABLE type_skills (
    type_id bigint PRIMARY KEY,
    skills jsonb NOT NULL,
    read_at timestamptz NOT NULL DEFAULT now()
);
