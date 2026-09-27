-- Doctrines apps share (allianceauth-fittings' doctrines, offered by
-- aa-fleetpings and aa-fat): one app publishes its list, which replaces the
-- last; Fleet Pings and apps approved to read them offer each to whoever
-- may see it. Approved by Jay, 2026-09-27.
CREATE TABLE core.shared_doctrines (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    -- The publisher's own id for it.
    key text NOT NULL CHECK (length(key) BETWEEN 1 AND 100),
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    -- Its page in the publishing app (a link path, checked by the host).
    link text NOT NULL CHECK (length(link) <= 200),
    -- Who sees it (AA's categories): NULL for everyone, else members of
    -- any of these groups.
    groups bigint[],
    -- The publisher's own permission whose holders see every doctrine
    -- (AA's fittings.manage), without the plugin.<id>. prefix.
    see_all text,
    position integer NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, key)
);
