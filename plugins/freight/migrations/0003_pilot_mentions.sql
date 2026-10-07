-- aa-freight's FREIGHT_DISCORD_MENTIONS (none by default): pilot notices
-- mention the Discord role mapped to this state, Tether's stand-in for
-- @here, @everyone or a role id. NULL mentions nobody.
ALTER TABLE settings ADD COLUMN pilot_ping text
    CHECK (char_length(pilot_ping) BETWEEN 1 AND 64);
-- When the last pilot notice went out without its mention (no Discord
-- role is mapped to that state), for the settings to say so; NULL once
-- one goes out with it, or the state is changed.
ALTER TABLE settings ADD COLUMN pilot_ping_refused timestamptz;

-- The state a queued message mentions; NULL for none.
ALTER TABLE outbox ADD COLUMN mention_state text;
