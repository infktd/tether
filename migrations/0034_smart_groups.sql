-- Secure Groups (aa-securegroups, AA's "Smart Groups"): groups whose members
-- Tether keeps by filters (all must pass; each can be reversed). Auto groups
-- add everyone who passes; others take requests from those who pass. Those
-- who stop passing are removed, after a grace period if the group has one.

-- A main's birthday, for the character age filter (public ESI, filled in
-- as needed).
ALTER TABLE core.characters
    ADD COLUMN birthday timestamptz,
    -- The last time ESI was asked, so failures go to the back of the queue.
    ADD COLUMN birthday_checked_at timestamptz;

CREATE TABLE core.smart_groups (
    group_id bigint PRIMARY KEY REFERENCES core.groups (id) ON DELETE CASCADE,
    auto_join boolean NOT NULL DEFAULT false,
    grace_days integer NOT NULL DEFAULT 0 CHECK (grace_days BETWEEN 0 AND 60),
    notify boolean NOT NULL DEFAULT true,
    swept_at timestamptz
);

CREATE TABLE core.smart_filters (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    group_id bigint NOT NULL REFERENCES core.smart_groups (group_id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN
        ('state', 'main_affiliation', 'any_affiliation', 'character_age', 'groups', 'compliant')),
    config jsonb NOT NULL DEFAULT '{}',
    reversed boolean NOT NULL DEFAULT false,
    added_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX smart_filters_group_idx ON core.smart_filters (group_id);

-- Members who stopped passing, and since when: removed when the grace
-- period ends, forgiven if they pass again.
CREATE TABLE core.smart_grace (
    group_id bigint NOT NULL REFERENCES core.smart_groups (group_id) ON DELETE CASCADE,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    since timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, account_id)
);
CREATE INDEX smart_grace_account_idx ON core.smart_grace (account_id);
