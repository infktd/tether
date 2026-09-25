-- Access states, Alliance Auth style (F4), replacing the fixed tiers.
-- Member, Blue (was Allied) and Guest are built in; admins add more.
CREATE TABLE core.states (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 40),
    -- Built-in states can't be renamed or deleted.
    builtin text UNIQUE CHECK (builtin IN ('member', 'blue', 'guest')),
    -- Higher wins. Guest is always 0, below everything; deferred so two
    -- states can swap places in one transaction.
    priority integer NOT NULL CHECK (priority >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT states_priority_key UNIQUE (priority) DEFERRABLE INITIALLY DEFERRED,
    CHECK ((builtin IS NOT DISTINCT FROM 'guest') = (priority = 0))
);
CREATE UNIQUE INDEX states_name_key ON core.states (lower(name));

INSERT INTO core.states (name, builtin, priority) VALUES
    ('Member', 'member', 2),
    ('Blue', 'blue', 1),
    ('Guest', 'guest', 0);

CREATE FUNCTION core.guest_state() RETURNS bigint
LANGUAGE sql STABLE AS $$ SELECT id FROM core.states WHERE builtin = 'guest' $$;

-- Which alliances, corporations and characters a state covers. An entity
-- may be in several states; the highest priority wins. EVE ids are unique
-- across entity types. Guest covers everyone else and lists nothing.
CREATE TABLE core.state_entities (
    state_id bigint NOT NULL REFERENCES core.states (id) ON DELETE CASCADE,
    entity_id bigint NOT NULL,
    entity_kind text NOT NULL CHECK (entity_kind IN ('alliance', 'corporation', 'character')),
    name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (state_id, entity_id)
);

INSERT INTO core.state_entities (state_id, entity_id, entity_kind, name, added_at)
SELECT s.id, r.entity_id, r.entity_kind, r.name, r.created_at
FROM core.tier_rules r
JOIN core.states s ON s.builtin = CASE r.tier WHEN 'allied' THEN 'blue' ELSE r.tier END;

DROP TABLE core.tier_rules;

-- Accounts: the state replaces the tier. Deleting a state is refused while
-- accounts are in it; Tether moves them first.
ALTER TABLE core.accounts ADD COLUMN state_id bigint REFERENCES core.states (id);
UPDATE core.accounts a SET state_id = s.id
FROM core.states s
WHERE s.builtin = CASE a.tier WHEN 'allied' THEN 'blue' ELSE a.tier END;
ALTER TABLE core.accounts
    ALTER COLUMN state_id SET DEFAULT core.guest_state(),
    ALTER COLUMN state_id SET NOT NULL;
CREATE INDEX accounts_state_idx ON core.accounts (state_id);

DROP TRIGGER discord_account_changed ON core.accounts;
ALTER TABLE core.accounts DROP COLUMN tier;
ALTER TABLE core.accounts RENAME COLUMN tier_evaluated_at TO state_evaluated_at;
CREATE TRIGGER discord_account_changed
    AFTER UPDATE OF state_id, main_character_id ON core.accounts
    FOR EACH ROW
    WHEN (OLD.state_id IS DISTINCT FROM NEW.state_id
          OR OLD.main_character_id IS DISTINCT FROM NEW.main_character_id)
    EXECUTE FUNCTION core.discord_account_changed();

-- Permission grants: to states or groups. Dropping the tier column drops
-- the old CHECK and UNIQUE with it.
ALTER TABLE core.permission_grants
    ADD COLUMN state_id bigint REFERENCES core.states (id) ON DELETE CASCADE;
UPDATE core.permission_grants g SET state_id = s.id
FROM core.states s
WHERE g.tier IS NOT NULL
  AND s.builtin = CASE g.tier WHEN 'allied' THEN 'blue' ELSE g.tier END;
ALTER TABLE core.permission_grants DROP COLUMN tier;
ALTER TABLE core.permission_grants
    ADD CONSTRAINT permission_grants_grantee_check CHECK ((state_id IS NULL) <> (group_id IS NULL)),
    ADD CONSTRAINT permission_grants_key UNIQUE NULLS NOT DISTINCT (permission, state_id, group_id);
UPDATE core.permission_grants SET permission = 'admin.states' WHERE permission = 'admin.tiers';

-- Discord role mappings: the same.
ALTER TABLE core.discord_role_mappings
    ADD COLUMN state_id bigint REFERENCES core.states (id) ON DELETE CASCADE;
UPDATE core.discord_role_mappings m SET state_id = s.id
FROM core.states s
WHERE m.tier IS NOT NULL
  AND s.builtin = CASE m.tier WHEN 'allied' THEN 'blue' ELSE m.tier END;
ALTER TABLE core.discord_role_mappings DROP COLUMN tier;
ALTER TABLE core.discord_role_mappings
    ADD CONSTRAINT discord_role_mappings_grantee_check CHECK ((state_id IS NULL) <> (group_id IS NULL)),
    ADD CONSTRAINT discord_role_mappings_key UNIQUE NULLS NOT DISTINCT (role_id, state_id, group_id);

-- Queued jobs keep working under their new kind names.
UPDATE core.jobs SET kind = 'states.' || substr(kind, length('tiers.') + 1)
WHERE kind LIKE 'tiers.%';
