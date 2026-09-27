-- Core rules as Alliance Auth's (Jay, 2026-09-27: "take AA's permissions
-- and their settings").

-- Any number of superusers (AA's is_superuser). `is_owner` now marks a
-- superuser: setup makes the first, and superusers grant and revoke it.
-- The name stays; only the one-owner index goes.
DROP INDEX core.accounts_single_owner_idx;

-- Permissions granted to single users (AA's user_permissions), beside
-- states and groups. Exactly one grantee per grant.
ALTER TABLE core.permission_grants
    ADD COLUMN account_id bigint REFERENCES core.accounts (id) ON DELETE CASCADE,
    DROP CONSTRAINT permission_grants_grantee_check,
    DROP CONSTRAINT permission_grants_key;
ALTER TABLE core.permission_grants
    ADD CONSTRAINT permission_grants_grantee_check
        CHECK (num_nonnulls(state_id, group_id, account_id) = 1),
    ADD CONSTRAINT permission_grants_key
        UNIQUE NULLS NOT DISTINCT (permission, state_id, group_id, account_id);
CREATE INDEX permission_grants_account_idx
    ON core.permission_grants (account_id) WHERE account_id IS NOT NULL;

-- AA's State.public: "Make this state available to any character" (with a
-- main). Never Guest (everyone already) or the Blacklist.
ALTER TABLE core.states
    ADD COLUMN public boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT states_public_check
        CHECK (NOT public OR builtin IS NULL OR builtin IN ('member', 'blue'));

-- AA's DISCORD_SYNC_NAMES is off by default, and now Tether's is too. An
-- existing instance that never saved the setting has been setting
-- nicknames: it keeps doing so.
INSERT INTO core.settings (key, value)
SELECT 'discord.sync_names', 'true'::jsonb
WHERE EXISTS (SELECT 1 FROM core.accounts)
   OR EXISTS (SELECT 1 FROM core.settings WHERE key LIKE 'discord.%')
ON CONFLICT (key) DO NOTHING;
