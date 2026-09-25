-- Login and ownership, Alliance Auth style.

-- An account can be without a main, as in AA: when its main is sold or
-- loses its last valid token. It is Guest, and only the Dashboard and
-- Change Main work, until its owner signs in with one of its characters.
ALTER TABLE core.accounts ALTER COLUMN main_character_id DROP NOT NULL;

-- Deactivated accounts (AA's inactive users): Guest, no permissions, no
-- sessions, sign-in refused. The owner can't be deactivated.
ALTER TABLE core.accounts
    ADD COLUMN active boolean NOT NULL DEFAULT true,
    ADD COLUMN deactivated_at timestamptz,
    ADD COLUMN deactivated_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    ADD CONSTRAINT accounts_owner_active CHECK (active OR NOT is_owner);

-- Every owner a character has had on each account (AA's OwnershipRecord),
-- newest last. A returning owner (same character, same owner hash) is
-- re-attached to the account it last belonged to.
CREATE TABLE core.ownership_records (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    owner_hash text NOT NULL,
    account_id bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ownership_records_lookup_idx
    ON core.ownership_records (character_id, owner_hash, id DESC);

INSERT INTO core.ownership_records (character_id, character_name, owner_hash, account_id, recorded_at)
SELECT id, name, owner_hash, account_id, added_at
FROM core.characters
WHERE owner_hash IS NOT NULL;

-- Sessions last 14 days from sign-in (Django's default, as AA).
UPDATE core.sessions SET expires_at = LEAST(expires_at, created_at + interval '14 days');

-- The ownership check replaces the daily token check: every token every
-- 4 hours, owner hash included.
DELETE FROM core.schedules WHERE name = 'compliance.check_tokens';
UPDATE core.jobs SET kind = 'ownership.check' WHERE kind = 'compliance.check_tokens';
