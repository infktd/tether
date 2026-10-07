-- aa-memberaudit's skill sets (memberaudit/models/general.py): a
-- description, a ship (for show), whether pilots see it on their own
-- sheets, and who changed it last; each skill with a required level, a
-- recommended level, or both.
ALTER TABLE skill_sets ADD COLUMN description text NOT NULL DEFAULT ''
    CHECK (length(description) <= 2000);
ALTER TABLE skill_sets ADD COLUMN ship_type_id bigint;
ALTER TABLE skill_sets ADD COLUMN is_visible boolean NOT NULL DEFAULT true;
ALTER TABLE skill_sets ADD COLUMN modified_at timestamptz;
ALTER TABLE skill_sets ADD COLUMN modified_by text;

ALTER TABLE skill_set_skills RENAME COLUMN level TO required_level;
ALTER TABLE skill_set_skills ALTER COLUMN required_level DROP NOT NULL;
ALTER TABLE skill_set_skills ADD COLUMN recommended_level integer
    CHECK (recommended_level BETWEEN 1 AND 5);
ALTER TABLE skill_set_skills ADD CONSTRAINT skill_set_skills_a_level
    CHECK (required_level IS NOT NULL OR recommended_level IS NOT NULL);

-- aa-memberaudit's skill set groups, doctrines among them: the sheet and
-- the reports group skill sets by them.
CREATE TABLE skill_set_groups (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 100),
    description text NOT NULL DEFAULT '' CHECK (length(description) <= 2000),
    is_doctrine boolean NOT NULL DEFAULT false,
    is_active boolean NOT NULL DEFAULT true,
    modified_at timestamptz,
    modified_by text
);

CREATE TABLE skill_set_group_sets (
    group_id bigint NOT NULL REFERENCES skill_set_groups ON DELETE CASCADE,
    set_id bigint NOT NULL REFERENCES skill_sets ON DELETE CASCADE,
    PRIMARY KEY (group_id, set_id)
);
CREATE INDEX skill_set_group_sets_set_idx ON skill_set_group_sets (set_id);
