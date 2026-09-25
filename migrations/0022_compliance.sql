-- Scope compliance (F11, F16, N8) and Corp Stats.

-- Scopes an admin requires for a state, on top of those Tether and (for
-- Member) the installed plugins need. Guest requires nothing.
CREATE TABLE core.state_scopes (
    state_id bigint NOT NULL REFERENCES core.states (id) ON DELETE CASCADE,
    scope text NOT NULL CHECK (scope ~ '^esi-[a-z_]+\.[a-z_]+\.v[0-9]+$'),
    added_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (state_id, scope)
);

-- Each installed plugin's user scopes, from its manifest: Member requires
-- them. Kept here so evaluation doesn't need the running plugins.
ALTER TABLE core.plugins ADD COLUMN user_scopes text[] NOT NULL DEFAULT '{}';

-- Compliance is a flag, as in Alliance Auth: an account keeps the state
-- its main's affiliation gives it; when not every character is registered
-- with that state's scopes it's flagged for officers and shown what to
-- register. Guest requires nothing, so Guests are always compliant.
ALTER TABLE core.accounts ADD COLUMN compliant boolean NOT NULL DEFAULT true;
CREATE INDEX accounts_not_compliant_idx ON core.accounts (id) WHERE NOT compliant;

-- Groups Tether manages itself. `compliant`: every compliant account in a
-- state other than Guest (like Member Audit's compliance groups), so
-- admins can make permissions and Discord roles depend on compliance.
ALTER TABLE core.groups ADD COLUMN managed text UNIQUE CHECK (managed IN ('compliant'));
ALTER TABLE core.groups ADD CONSTRAINT groups_managed_assigned
    CHECK (managed IS NULL OR join_policy = 'assigned');
INSERT INTO core.groups (name, description, join_policy, managed)
VALUES (
    'Compliant',
    'Everyone in a state other than Guest with every character registered. Tether keeps it up to date; grant it permissions or Discord roles to require compliance for them.',
    'assigned',
    'compliant'
)
ON CONFLICT (name) DO UPDATE SET managed = 'compliant', join_policy = 'assigned';
-- A group already called Compliant becomes the managed one: it starts
-- empty and evaluation fills it.
DELETE FROM core.group_members
WHERE group_id = (SELECT id FROM core.groups WHERE managed = 'compliant');
DELETE FROM core.group_requests
WHERE group_id = (SELECT id FROM core.groups WHERE managed = 'compliant');

-- Registering characters with the required scopes replaces per-plugin
-- consent.
DROP TABLE core.plugin_consents;
DELETE FROM core.login_attempts WHERE purpose = 'consent';
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'data_source', 'corp_source'));

-- When the daily token check last confirmed a token still works.
ALTER TABLE core.character_tokens ADD COLUMN checked_at timestamptz;

-- Corp Stats: characters whose owners offered their corporation's member
-- list, used once an admin approves, and only while the character stays
-- in the corporation it was approved for.
CREATE TABLE core.corp_sources (
    character_id bigint PRIMARY KEY REFERENCES core.characters (id) ON DELETE CASCADE,
    offered_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    offered_at timestamptz NOT NULL DEFAULT now(),
    approved_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    approved_at timestamptz,
    corporation_id bigint
);

-- The latest member list of each corporation with an approved source.
CREATE TABLE core.corp_member_lists (
    corporation_id bigint PRIMARY KEY,
    fetched_at timestamptz NOT NULL DEFAULT now(),
    members integer NOT NULL
);
CREATE TABLE core.corp_members (
    corporation_id bigint NOT NULL REFERENCES core.corp_member_lists (corporation_id) ON DELETE CASCADE,
    character_id bigint NOT NULL,
    PRIMARY KEY (corporation_id, character_id)
);
