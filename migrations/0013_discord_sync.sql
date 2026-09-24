-- Corporation and alliance tickers, for the Discord nickname template.
-- Tickers rarely change; they are refetched when older than a day.
CREATE TABLE core.entity_tickers (
    id bigint PRIMARY KEY,
    ticker text NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now()
);

-- Queues a Discord sync for an account, if it has linked Discord and no
-- sync for it is already waiting. Triggers below call it whenever
-- something a member's roles or nickname depend on changes, so no code
-- path can forget.
--
-- The waiting job counts only if we can lock it: one a worker is claiming
-- right now may read the old state, so then a new job is queued. And while
-- this transaction holds it, workers skip it until the change commits.
CREATE FUNCTION core.discord_queue_sync(account bigint) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO core.jobs (kind, payload, max_attempts)
    SELECT 'discord.sync_member', jsonb_build_object('account_id', account), 10
    WHERE EXISTS (SELECT 1 FROM core.discord_links WHERE account_id = account)
      AND NOT EXISTS (
          SELECT 1 FROM core.jobs
          WHERE kind = 'discord.sync_member' AND state = 'queued'
            AND payload = jsonb_build_object('account_id', account)
          FOR UPDATE SKIP LOCKED
      );
$$;

-- Tier or main changed.
CREATE FUNCTION core.discord_account_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM core.discord_queue_sync(NEW.id);
    RETURN NULL;
END;
$$;
CREATE TRIGGER discord_account_changed
    AFTER UPDATE OF tier, main_character_id ON core.accounts
    FOR EACH ROW
    WHEN (OLD.tier IS DISTINCT FROM NEW.tier
          OR OLD.main_character_id IS DISTINCT FROM NEW.main_character_id)
    EXECUTE FUNCTION core.discord_account_changed();

-- Joined or left a group.
CREATE FUNCTION core.discord_membership_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM core.discord_queue_sync(OLD.account_id);
    ELSE
        PERFORM core.discord_queue_sync(NEW.account_id);
    END IF;
    RETURN NULL;
END;
$$;
CREATE TRIGGER discord_membership_changed
    AFTER INSERT OR DELETE ON core.group_members
    FOR EACH ROW EXECUTE FUNCTION core.discord_membership_changed();

-- The main's name, corporation or alliance changed (the nickname uses them).
CREATE FUNCTION core.discord_main_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM core.discord_queue_sync(a.id)
    FROM core.accounts a
    WHERE a.id = NEW.account_id AND a.main_character_id = NEW.id;
    RETURN NULL;
END;
$$;
CREATE TRIGGER discord_main_changed
    AFTER UPDATE OF name, corporation_id, alliance_id ON core.characters
    FOR EACH ROW
    WHEN (OLD.name IS DISTINCT FROM NEW.name
          OR OLD.corporation_id IS DISTINCT FROM NEW.corporation_id
          OR OLD.alliance_id IS DISTINCT FROM NEW.alliance_id)
    EXECUTE FUNCTION core.discord_main_changed();

-- A mapping was added or removed (directly, or by deleting its group):
-- every linked member is synced, and a removed role is taken back from
-- whoever no longer gets it through another mapping.
CREATE FUNCTION core.discord_mapping_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO core.jobs (kind, payload, max_attempts)
    VALUES (
        'discord.sync_all',
        CASE WHEN TG_OP = 'DELETE'
            THEN jsonb_build_object('removed_role_id', OLD.role_id)
            ELSE '{}'::jsonb
        END,
        10
    );
    RETURN NULL;
END;
$$;
CREATE TRIGGER discord_mapping_changed
    AFTER INSERT OR DELETE ON core.discord_role_mappings
    FOR EACH ROW EXECUTE FUNCTION core.discord_mapping_changed();
