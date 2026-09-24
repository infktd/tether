-- The publisher key each plugin's packages must be signed with, pinned on
-- first install. It changes only through a rotation the pinned key signed
-- or an admin re-pin after a confirmation step, both audited. Kept after
-- uninstall, so a later package under the same id from someone else is
-- still caught.
CREATE TABLE core.plugin_keys (
    plugin_id text PRIMARY KEY,
    public_key text NOT NULL,
    -- How the current key got here.
    pinned_by text NOT NULL CHECK (pinned_by IN ('first_install', 'rotation', 'repin')),
    pinned_at timestamptz NOT NULL DEFAULT now()
);
