-- Services, Alliance Auth style (F12, F23).

-- Discord access by permission (AA's "Can access the Discord service").
-- Every state but Guest had it until now, so each keeps it; a new
-- instance has only Member, Blue and Guest here, so that's Member and
-- Blue by default.
INSERT INTO core.permission_grants (permission, state_id)
SELECT 'discord.access_discord', id FROM core.states WHERE builtin IS DISTINCT FROM 'guest'
ON CONFLICT DO NOTHING;

-- Who may use Discord just changed: check every linked member (losing
-- access unlinks and removes them from the server).
CREATE FUNCTION core.discord_access_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF COALESCE(NEW.permission, OLD.permission) = 'discord.access_discord' THEN
        INSERT INTO core.jobs (kind, payload, max_attempts)
        VALUES ('discord.sync_all', '{}', 10);
    END IF;
    RETURN NULL;
END;
$$;
CREATE TRIGGER discord_access_changed
    AFTER INSERT OR DELETE ON core.permission_grants
    FOR EACH ROW EXECUTE FUNCTION core.discord_access_changed();

-- However a link goes (unlinking, losing access, relinking, the account
-- being deleted), the Discord user leaves the server, as in AA.
CREATE OR REPLACE FUNCTION core.discord_link_removed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO core.jobs (kind, payload, max_attempts)
    VALUES (
        'discord.remove_member',
        jsonb_build_object('discord_user_id', OLD.discord_user_id),
        10
    );
    RETURN NULL;
END;
$$;
UPDATE core.jobs SET kind = 'discord.remove_member' WHERE kind = 'discord.strip_roles';

-- AA's Name Formatter: one nickname format per state. States without one
-- use AA's default, {character_name}. The old single template carries
-- over to every state, in AA's field names.
CREATE TABLE core.discord_name_formats (
    state_id bigint PRIMARY KEY REFERENCES core.states (id) ON DELETE CASCADE,
    format text NOT NULL CHECK (length(format) BETWEEN 1 AND 100)
);
INSERT INTO core.discord_name_formats (state_id, format)
SELECT s.id, f.format
FROM core.states s
CROSS JOIN (
    SELECT replace(replace(replace(trim(value #>> '{}'), '{name}', '{character_name}'),
                           '{corp}', '{corp_ticker}'),
                   '{alliance}', '{alliance_ticker}') AS format
    FROM core.settings WHERE key = 'discord.nickname_template'
) f
-- One too long in the new names keeps AA's default instead.
WHERE length(f.format) BETWEEN 1 AND 100;
-- AA's DISCORD_SYNC_NAMES: whether Tether sets nicknames at all. Off if
-- this instance had no template (it left nicknames alone), on otherwise.
INSERT INTO core.settings (key, value)
SELECT 'discord.sync_names', to_jsonb(EXISTS (
    SELECT 1 FROM core.settings
    WHERE key = 'discord.nickname_template' AND length(trim(value #>> '{}')) > 0
))
WHERE EXISTS (SELECT 1 FROM core.accounts);
DELETE FROM core.settings WHERE key = 'discord.nickname_template';
