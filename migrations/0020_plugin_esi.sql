-- Plugin ESI access (F16, N8, N10).

-- Per-user consent: a character's owner let a plugin use its user scopes
-- (`capabilities.esi.user`). Revocable from the profile page.
CREATE TABLE core.plugin_consents (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    character_id bigint NOT NULL REFERENCES core.characters (id) ON DELETE CASCADE,
    scopes text[] NOT NULL,
    granted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, character_id)
);

-- Data-source characters (`capabilities.esi.data_source`): offered by the
-- character's owner, used only once an admin approves.
CREATE TABLE core.plugin_data_sources (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    character_id bigint NOT NULL REFERENCES core.characters (id) ON DELETE CASCADE,
    offered_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    offered_at timestamptz NOT NULL DEFAULT now(),
    approved_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    approved_at timestamptz,
    -- The corporation the admin approved reading. If the character moves,
    -- it stops being used until approved again.
    corporation_id bigint,
    PRIMARY KEY (plugin_id, character_id)
);

-- Every ESI call and Discord post a plugin makes, shown on its admin page
-- and kept 90 days (capped per plugin): after an uninstall too, since a
-- plugin that gets removed is the one whose record matters. (Consents,
-- offers and approvals are in the audit log.)
CREATE TABLE core.plugin_access_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plugin_id text NOT NULL,
    character_id bigint,
    endpoint text NOT NULL,
    -- ok, or why it was refused or failed
    outcome text NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX plugin_access_log_recent_idx ON core.plugin_access_log (plugin_id, id DESC);
CREATE INDEX plugin_access_log_at_idx ON core.plugin_access_log (at);

-- Discord channels an admin lets a plugin post to.
CREATE TABLE core.plugin_channels (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    channel_id bigint NOT NULL,
    PRIMARY KEY (plugin_id, channel_id)
);

-- Why a login was started: a plain login, or granting a plugin scopes
-- (consent), or offering a character as a plugin's data source. It asks
-- SSO for these scopes.
ALTER TABLE core.login_attempts ADD COLUMN scopes text[] NOT NULL DEFAULT '{}';
ALTER TABLE core.login_attempts ADD COLUMN purpose text NOT NULL DEFAULT 'login'
    CHECK (purpose IN ('login', 'consent', 'data_source'));
ALTER TABLE core.login_attempts ADD COLUMN plugin_id text;
-- The account that started a consent or offer login: only it may finish.
ALTER TABLE core.login_attempts ADD COLUMN started_by bigint;
