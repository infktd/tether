-- States, AA behaviour: priorities are AA's numbers (Member 100, Blue 50,
-- Guest 0), editable; the relative order of existing states is kept.
UPDATE core.states SET priority = priority * 50 WHERE priority > 0;

-- AA's state names are at most 32 characters. Longer names are cut and
-- given their id, so two can't end up the same.
UPDATE core.states SET name = rtrim(left(name, 31 - length(id::text))) || '~' || id
WHERE length(name) > 32;
ALTER TABLE core.states DROP CONSTRAINT states_name_check;
ALTER TABLE core.states ADD CONSTRAINT states_name_check CHECK (length(name) BETWEEN 1 AND 32);
