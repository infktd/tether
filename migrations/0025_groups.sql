-- Groups, Alliance Auth style (F5, F23).

-- AA's flags replace the join policy: Assigned becomes Internal (and
-- Hidden, as AA's defaults), Open stays Open and listed, Request to join
-- becomes a listed Requestable group.
ALTER TABLE core.groups
    ADD COLUMN internal boolean NOT NULL DEFAULT true,
    ADD COLUMN hidden boolean NOT NULL DEFAULT true,
    ADD COLUMN open boolean NOT NULL DEFAULT false,
    ADD COLUMN public boolean NOT NULL DEFAULT false,
    ADD COLUMN restricted boolean NOT NULL DEFAULT false,
    -- Member Audit's compliance groups: an Internal group Tether keeps
    -- filled with the compliant accounts of its allowed states.
    ADD COLUMN compliance boolean NOT NULL DEFAULT false;
UPDATE core.groups SET
    internal = (join_policy = 'assigned'),
    hidden = (join_policy = 'assigned'),
    open = (join_policy = 'open');
UPDATE core.groups SET compliance = true, internal = true WHERE managed = 'compliant';
ALTER TABLE core.groups DROP CONSTRAINT groups_managed_assigned;
ALTER TABLE core.groups DROP COLUMN managed;
ALTER TABLE core.groups DROP COLUMN join_policy;
ALTER TABLE core.groups ADD CONSTRAINT groups_compliance_internal CHECK (internal OR NOT compliance);
UPDATE core.groups SET description = left(description, 512) WHERE length(description) > 512;
ALTER TABLE core.groups ADD CONSTRAINT groups_description_length CHECK (length(description) <= 512);

-- Only these states may be in the group (none listed: every state).
CREATE TABLE core.group_states (
    group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    state_id bigint NOT NULL REFERENCES core.states (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, state_id)
);

-- Group Leaders, and Group Leader Groups (anyone in them leads).
CREATE TABLE core.group_leaders (
    group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, account_id)
);
CREATE TABLE core.group_leader_groups (
    group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    leader_group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, leader_group_id),
    CHECK (group_id <> leader_group_id)
);

-- Requests to leave, as well as to join.
ALTER TABLE core.group_requests ADD COLUMN leave boolean NOT NULL DEFAULT false;

-- AA's per-group Audit Log (RequestLog), with name snapshots so entries
-- stay readable, and no foreign keys so they outlive what they describe.
CREATE TABLE core.group_request_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    group_id bigint NOT NULL,
    group_name text NOT NULL,
    request_type text NOT NULL CHECK (request_type IN ('join', 'leave', 'removed')),
    action text NOT NULL CHECK (action IN ('accept', 'reject')),
    requestor_account_id bigint,
    requestor_main text,
    requestor_corporation_id bigint,
    actor_account_id bigint,
    actor_name text,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX group_request_log_group_idx ON core.group_request_log (group_id, id DESC);

-- Names groups can't take (case-insensitive); Discord leaves roles with
-- them alone.
CREATE TABLE core.reserved_group_names (
    name text PRIMARY KEY CHECK (name = lower(name)),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 200),
    created_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- AA's permissions. request_groups goes to Member (AA's docs); whoever
-- could manage groups before keeps Group Management.
INSERT INTO core.permission_grants (permission, state_id)
SELECT 'request_groups', id FROM core.states WHERE builtin = 'member'
ON CONFLICT DO NOTHING;
INSERT INTO core.permission_grants (permission, state_id, group_id)
SELECT 'group_management', state_id, group_id FROM core.permission_grants
WHERE permission = 'admin.groups'
ON CONFLICT DO NOTHING;
