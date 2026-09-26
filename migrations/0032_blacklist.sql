-- Blacklist and Pilot Log (as AA's blacklist app). An account with any
-- character that is, or is in a corporation or alliance that is,
-- blacklisted is in the Blacklist state, above every other: no
-- permissions, no groups, no services. Never the owner. Notes on any
-- pilot, corporation or alliance are the Pilot Log.

ALTER TABLE core.states DROP CONSTRAINT states_builtin_check;
ALTER TABLE core.states ADD CONSTRAINT states_builtin_check
    CHECK (builtin IN ('member', 'blue', 'guest', 'blacklist'));
-- An admin-made state may already be called that: it keeps its members
-- and grants under a new name.
UPDATE core.states SET name = left(name, 26) || ' (old)'
WHERE lower(name) = 'blacklist' AND builtin IS NULL;
-- Above every state (evaluation doesn't rely on it: see core.blacklisted).
INSERT INTO core.states (name, builtin, priority)
    SELECT 'Blacklist', 'blacklist', GREATEST(1000001, (SELECT max(priority) + 1 FROM core.states))
    WHERE NOT EXISTS (SELECT 1 FROM core.states WHERE builtin = 'blacklist');

CREATE TABLE core.blacklist (
    entity_id bigint PRIMARY KEY,
    entity_kind text NOT NULL CHECK (entity_kind IN ('alliance', 'corporation', 'character')),
    name text NOT NULL,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 1000),
    added_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    added_by_name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now()
);

-- Whether an account is blacklisted: any of its characters is listed, or
-- is in a listed corporation or alliance. Never the owner. Checked where
-- permissions, groups and leadership are decided, so it applies the
-- moment an entry is added, before the account's state moves.
CREATE FUNCTION core.blacklisted(account bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT COALESCE((
        SELECT NOT a.is_owner AND EXISTS (
            SELECT 1 FROM core.characters c
            JOIN core.blacklist b ON b.entity_id IN (c.id, c.corporation_id, c.alliance_id)
            WHERE c.account_id = a.id)
        FROM core.accounts a WHERE a.id = account), false)
$$;

CREATE TABLE core.pilot_notes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entity_id bigint NOT NULL,
    entity_kind text NOT NULL CHECK (entity_kind IN ('alliance', 'corporation', 'character')),
    name text NOT NULL,
    note text NOT NULL CHECK (length(note) BETWEEN 1 AND 2000),
    added_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    added_by_name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pilot_notes_entity_idx ON core.pilot_notes (entity_id, added_at DESC);
