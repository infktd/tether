-- Instance secrets (the Discord bot token and client secret), sealed with
-- ENCRYPTION_KEY. The associated data is "secret:<name>", so a sealed value
-- can't be moved to another name.
CREATE TABLE core.secrets (
    name text PRIMARY KEY,
    sealed bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- One Discord user per account, and one account per Discord user.
CREATE TABLE core.discord_links (
    account_id bigint PRIMARY KEY REFERENCES core.accounts (id) ON DELETE CASCADE,
    discord_user_id bigint NOT NULL UNIQUE,
    username text NOT NULL,
    linked_at timestamptz NOT NULL DEFAULT now()
);

-- A link between the redirect to Discord and the callback. Single use, bound
-- to the browser that started it and the account it links.
CREATE TABLE core.discord_link_attempts (
    state text PRIMARY KEY,
    browser_hash bytea NOT NULL,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX discord_link_attempts_expires_idx ON core.discord_link_attempts (expires_at);

-- Discord roles given to a tier or a group. role_name is as of mapping, for
-- display when Discord can't be asked.
CREATE TABLE core.discord_role_mappings (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    role_id bigint NOT NULL,
    role_name text NOT NULL,
    tier text CHECK (tier IN ('member', 'allied', 'guest')),
    group_id bigint REFERENCES core.groups (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((tier IS NULL) <> (group_id IS NULL)),
    UNIQUE NULLS NOT DISTINCT (role_id, tier, group_id)
);

-- However a link goes (unlinking, relinking, the account being deleted),
-- the Discord user loses the roles Tether gave them, through the job queue
-- so a Discord outage only delays it.
CREATE FUNCTION core.discord_link_removed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO core.jobs (kind, payload, max_attempts)
    VALUES (
        'discord.strip_roles',
        jsonb_build_object('discord_user_id', OLD.discord_user_id),
        10
    );
    RETURN NULL;
END;
$$;

CREATE TRIGGER discord_link_deleted
    AFTER DELETE ON core.discord_links
    FOR EACH ROW EXECUTE FUNCTION core.discord_link_removed();
CREATE TRIGGER discord_link_changed
    AFTER UPDATE OF discord_user_id ON core.discord_links
    FOR EACH ROW
    WHEN (OLD.discord_user_id IS DISTINCT FROM NEW.discord_user_id)
    EXECUTE FUNCTION core.discord_link_removed();
