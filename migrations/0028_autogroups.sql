-- Auto Groups, Alliance Auth style (F23): for chosen states, a group for
-- each main's corporation and alliance, kept by Tether.

CREATE TABLE core.autogroup_configs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    corp_groups boolean NOT NULL DEFAULT true,
    corp_prefix text NOT NULL DEFAULT 'Corp ' CHECK (length(corp_prefix) <= 30),
    corp_source text NOT NULL DEFAULT 'name' CHECK (corp_source IN ('name', 'ticker')),
    alliance_groups boolean NOT NULL DEFAULT false,
    alliance_prefix text NOT NULL DEFAULT 'Alliance ' CHECK (length(alliance_prefix) <= 30),
    alliance_source text NOT NULL DEFAULT 'name' CHECK (alliance_source IN ('name', 'ticker')),
    -- Replace spaces in the corporation or alliance part with this.
    replace_spaces boolean NOT NULL DEFAULT false,
    replace_with text NOT NULL DEFAULT '' CHECK (length(replace_with) <= 5),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The states a config covers.
CREATE TABLE core.autogroup_config_states (
    config_id bigint NOT NULL REFERENCES core.autogroup_configs (id) ON DELETE CASCADE,
    state_id bigint NOT NULL REFERENCES core.states (id) ON DELETE CASCADE,
    PRIMARY KEY (config_id, state_id)
);

-- The groups a config keeps, one per corporation or alliance.
CREATE TABLE core.autogroup_groups (
    group_id bigint PRIMARY KEY REFERENCES core.groups (id) ON DELETE CASCADE,
    config_id bigint NOT NULL REFERENCES core.autogroup_configs (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('corporation', 'alliance')),
    entity_id bigint NOT NULL,
    UNIQUE (config_id, kind, entity_id)
);

-- A config's groups go with it (AA).
CREATE FUNCTION core.autogroup_removed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM core.groups WHERE id = OLD.group_id;
    RETURN NULL;
END;
$$;
CREATE TRIGGER autogroup_removed
    AFTER DELETE ON core.autogroup_groups
    FOR EACH ROW EXECUTE FUNCTION core.autogroup_removed();
