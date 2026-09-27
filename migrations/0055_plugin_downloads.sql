-- Files apps offer for download (aa-memberaudit's data exports; approved by
-- Jay, 2026-09-27): the app hands over rows, the host writes the CSV and
-- keeps it in parts; the finished version is served to holders of the
-- app's permission named with it, while the next one is built beside it.
CREATE TABLE core.plugin_downloads (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (name ~ '^[a-z0-9-]{1,50}$'),
    -- The finished file people get.
    ready_version integer,
    ready_title text,
    ready_permission text,
    ready_rows bigint NOT NULL DEFAULT 0,
    ready_bytes bigint NOT NULL DEFAULT 0,
    built_at timestamptz,
    -- The one being built.
    building_version integer,
    building_title text,
    building_permission text,
    building_columns integer NOT NULL DEFAULT 0,
    building_rows bigint NOT NULL DEFAULT 0,
    building_bytes bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (plugin_id, name)
);

CREATE TABLE core.plugin_download_parts (
    plugin_id text NOT NULL,
    name text NOT NULL,
    version integer NOT NULL,
    seq integer NOT NULL,
    csv text NOT NULL,
    PRIMARY KEY (plugin_id, name, version, seq),
    FOREIGN KEY (plugin_id, name) REFERENCES core.plugin_downloads (plugin_id, name)
        ON DELETE CASCADE
);
