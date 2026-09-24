-- Discord channels fleet pings may go to, chosen by an admin.
-- guild_id ties them to a server: switching Tether to another server
-- leaves the old ones unused.
CREATE TABLE core.discord_ping_channels (
    channel_id bigint PRIMARY KEY,
    guild_id bigint NOT NULL,
    name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now()
);

-- Every fleet ping, as sent (or tried). Names are kept as they were, so the
-- history reads right after channels or roles are renamed.
CREATE TABLE core.fleet_pings (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    sender_name text NOT NULL,
    channel_id bigint NOT NULL,
    channel_name text NOT NULL,
    -- none, here, everyone or role
    target text NOT NULL CHECK (target IN ('none', 'here', 'everyone', 'role')),
    role_id bigint,
    role_name text,
    message text NOT NULL,
    -- Random, so Discord can drop a repeated send without colliding with
    -- another instance sharing the bot.
    nonce text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    discord_message_id bigint,
    failed_at timestamptz,
    error text,
    CHECK ((target = 'role') = (role_id IS NOT NULL))
);
CREATE INDEX fleet_pings_recent_idx ON core.fleet_pings (created_at DESC);
CREATE INDEX fleet_pings_account_idx ON core.fleet_pings (account_id, created_at);
