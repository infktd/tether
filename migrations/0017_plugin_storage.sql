-- Plugin storage (N9). A plugin approved for storage gets a schema, owned
-- by Tether's role so the plugin can't share it, and a login role that can
-- only use and create objects in that schema.
CREATE TABLE core.plugin_storage (
    plugin_id text PRIMARY KEY REFERENCES core.plugins (id) ON DELETE CASCADE,
    schema_name text NOT NULL UNIQUE,
    -- Roles belong to the whole Postgres cluster, so the name carries a
    -- random part: two databases (tests, or two instances) never collide.
    role_name text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A plugin's migrations as applied, with a checksum, so a package that
-- changes an applied migration is refused.
CREATE TABLE core.plugin_migrations (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    version integer NOT NULL,
    name text NOT NULL,
    sha256 bytea NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, version)
);

-- What every role gets by default (PUBLIC), and plugin roles mustn't have.
-- Tether's own role owns the database and is unaffected.
REVOKE ALL ON SCHEMA public FROM PUBLIC;
DO $$
BEGIN
    EXECUTE format('REVOKE CREATE, TEMPORARY ON DATABASE %I FROM PUBLIC', current_database());
END
$$;

-- Advisory locks are shared by the whole database, and Tether takes them
-- (signing in, Discord linking, migrations): a plugin holding one could
-- block those.
DO $$
DECLARE
    f regprocedure;
BEGIN
    FOR f IN
        SELECT p.oid::regprocedure FROM pg_proc p
        WHERE p.pronamespace = 'pg_catalog'::regnamespace AND p.proname LIKE 'pg\_%advisory%'
    LOOP
        EXECUTE format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', f);
    END LOOP;
END
$$;
