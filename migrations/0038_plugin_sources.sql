-- Apps from GitHub (F15, F18): the repository an upload came from, and for
-- an installed app, where newer versions are looked for and what the last
-- daily check found.
ALTER TABLE core.plugin_uploads
    ADD COLUMN source text CHECK (source ~ '^[A-Za-z0-9-]{1,39}/[A-Za-z0-9._-]{1,100}$');

ALTER TABLE core.plugins
    ADD COLUMN source text CHECK (source ~ '^[A-Za-z0-9-]{1,39}/[A-Za-z0-9._-]{1,100}$'),
    -- The newest version the last check found, and its release page.
    ADD COLUMN latest_version text,
    ADD COLUMN latest_url text,
    ADD COLUMN checked_at timestamptz,
    ADD COLUMN check_error text;
