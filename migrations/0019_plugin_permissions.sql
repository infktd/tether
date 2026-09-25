-- Permissions plugins declare, as plugin.<id>.<name>: grantable like core
-- ones while the plugin is installed. Uninstalling removes them and every
-- grant of them.
CREATE TABLE core.plugin_permissions (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    -- The full name, plugin.<id>.<name>.
    permission text NOT NULL UNIQUE,
    description text NOT NULL,
    PRIMARY KEY (plugin_id, permission)
);
