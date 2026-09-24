-- Groups add access on top of tiers (F5).
CREATE TABLE core.groups (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL UNIQUE,
    description text NOT NULL DEFAULT '',
    join_policy text NOT NULL CHECK (join_policy IN ('open', 'request', 'assigned')),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE core.group_members (
    group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, account_id)
);
CREATE INDEX group_members_account_idx ON core.group_members (account_id);

CREATE TABLE core.group_requests (
    group_id bigint NOT NULL REFERENCES core.groups (id) ON DELETE CASCADE,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    requested_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, account_id)
);

-- Permissions are granted to tiers or groups only, never to accounts (F6).
CREATE TABLE core.permission_grants (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    permission text NOT NULL,
    tier text CHECK (tier IN ('member', 'allied', 'guest')),
    group_id bigint REFERENCES core.groups (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((tier IS NULL) <> (group_id IS NULL)),
    UNIQUE NULLS NOT DISTINCT (permission, tier, group_id)
);

-- Append-only record of admin actions and access changes (F6, N10).
-- actor_name is a snapshot so entries stay readable if the account goes;
-- there is deliberately no foreign key, since that would need UPDATEs.
CREATE TABLE core.audit_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now(),
    actor_account_id bigint, -- NULL: the system
    actor_name text,
    action text NOT NULL,
    target text,
    details jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_log_at_idx ON core.audit_log (at DESC, id DESC);

CREATE FUNCTION core.audit_log_is_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'core.audit_log is append-only';
END;
$$;

CREATE TRIGGER audit_log_append_only
    BEFORE UPDATE OR DELETE ON core.audit_log
    FOR EACH ROW EXECUTE FUNCTION core.audit_log_is_append_only();
