-- Menu customization (AA's Menu): admins reorder, rename and hide sidebar
-- items, group them in sections and folders, and add custom links. No rows
-- is the default layout; items without a row keep their default place, so
-- new features and apps show up without anyone editing the menu.
CREATE TABLE core.menu_entries (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('section', 'folder', 'item', 'link')),
    -- Items: a built-in key or `plugin:<id>:<path>`; the default sections:
    -- `section:<name>`. Custom sections, folders and links have none.
    key text UNIQUE,
    -- Shown instead of the default; required for custom entries.
    label text CHECK (label IS NULL OR length(label) BETWEEN 1 AND 40),
    -- https, or a page here: `/` then anything but a second `/` or `\`
    -- (browsers read `//host` and `/\host` as another site).
    url text CHECK (url IS NULL OR ((url ~ '^https://' OR url ~ '^/([^/\\]|$)') AND length(url) <= 500)),
    new_tab boolean NOT NULL DEFAULT false,
    -- A section, or a folder in one. NULL: the item's default section.
    parent_id bigint REFERENCES core.menu_entries (id) ON DELETE SET NULL,
    position integer NOT NULL DEFAULT 0,
    hidden boolean NOT NULL DEFAULT false,
    CHECK (kind <> 'link' OR (url IS NOT NULL AND label IS NOT NULL)),
    CHECK (kind NOT IN ('folder') OR label IS NOT NULL),
    CHECK (kind <> 'section' OR parent_id IS NULL),
    CHECK ((kind = 'item') = (key IS NOT NULL AND key NOT LIKE 'section:%'))
);
