-- Plugin packages an admin uploaded, waiting for their approval. A row
-- exists only once the package, its signature, the pinned key and the
-- component have all checked out; approving checks them again. Pruned
-- after a day.
CREATE TABLE core.plugin_uploads (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plugin_id text NOT NULL,
    version text NOT NULL,
    package bytea NOT NULL,
    signature text NOT NULL,
    uploaded_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    uploaded_at timestamptz NOT NULL DEFAULT now()
);

-- Installed plugins. The signed package is kept whole: its manifest is
-- what the admin approved, and it is loaded from here at startup, only if
-- it still hashes to what was approved (also in the audit log) and is
-- signed with the pinned key.
CREATE TABLE core.plugins (
    id text PRIMARY KEY,
    name text NOT NULL,
    version text NOT NULL,
    package bytea NOT NULL,
    signature text NOT NULL,
    package_sha256 bytea NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    installed_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    installed_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Packages are zips already: don't spend time compressing them again.
ALTER TABLE core.plugin_uploads ALTER COLUMN package SET STORAGE EXTERNAL;
ALTER TABLE core.plugins ALTER COLUMN package SET STORAGE EXTERNAL;
