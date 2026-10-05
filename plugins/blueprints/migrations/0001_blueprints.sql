-- Where new requests are posted, and how the last reads went.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    channel text,
    blueprints_at timestamptz,
    jobs_at timestamptz,
    places_at timestamptz,
    sync_error text
);
INSERT INTO settings (id) VALUES (1);

-- Personal owners (aa-blueprints' add_personal_blueprint_owner): a
-- character registered for the app that its pilot, holding the permission,
-- added. Corporate owners are the app's data sources.
CREATE TABLE personal_owners (
    character_id bigint PRIMARY KEY,
    account_id bigint NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now()
);

-- Every owner whose blueprints were read: a corporation (its id) or a
-- character (its id), with the corporation and alliance who may see them.
CREATE TABLE owners (
    kind text NOT NULL CHECK (kind IN ('corporation', 'character')),
    id bigint NOT NULL,
    name text NOT NULL DEFAULT '',
    corporation_id bigint NOT NULL,
    alliance_id bigint,
    read_at timestamptz,
    error text,
    -- Since when it's no longer an owner (its data source gone, or the
    -- character no longer registered): hidden at once, and forgotten
    -- with its blueprints and requests a week later, so a blip loses
    -- nothing.
    missing_since timestamptz,
    PRIMARY KEY (kind, id)
);

-- Blueprints as ESI last said. runs is NULL for an original.
CREATE TABLE blueprints (
    item_id bigint PRIMARY KEY,
    owner_kind text NOT NULL,
    owner_id bigint NOT NULL,
    type_id bigint NOT NULL,
    location_id bigint NOT NULL,
    location_flag text NOT NULL,
    quantity integer NOT NULL,
    runs integer,
    material_efficiency integer NOT NULL,
    time_efficiency integer NOT NULL,
    -- Where it is: the station, structure or system at the top, and the
    -- containers and hangars between ([[type_id, flag], ...]), once read.
    place_id bigint,
    within jsonb,
    FOREIGN KEY (owner_kind, owner_id) REFERENCES owners (kind, id) ON DELETE CASCADE
);
CREATE INDEX blueprints_owner_idx ON blueprints (owner_kind, owner_id);

-- Running industry jobs on the blueprints (aa-blueprints' "in use").
CREATE TABLE jobs (
    job_id bigint PRIMARY KEY,
    item_id bigint NOT NULL REFERENCES blueprints (item_id) ON DELETE CASCADE,
    activity integer NOT NULL,
    installer_id bigint NOT NULL,
    runs integer NOT NULL,
    start_date timestamptz NOT NULL,
    end_date timestamptz NOT NULL,
    status text NOT NULL
);

-- Names of types, people and corporations; blueprints' products, for
-- their icon (the image server has no plain icon for blueprints).
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
CREATE TABLE products (
    blueprint_type_id bigint PRIMARY KEY,
    product_type_id bigint
);

-- Stations, structures and systems blueprints are in.
CREATE TABLE places (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    system_name text NOT NULL DEFAULT '',
    read_at timestamptz NOT NULL DEFAULT now()
);

-- Requests for copies (aa-blueprints' Request): open, in progress,
-- fulfilled or cancelled. They go with their blueprint, as in AA.
CREATE TABLE requests (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    item_id bigint NOT NULL REFERENCES blueprints (item_id) ON DELETE CASCADE,
    requester_account bigint NOT NULL,
    requester_id bigint NOT NULL,
    requester_name text NOT NULL,
    -- Runs per copy; NULL for as many as allowed.
    runs integer CHECK (runs > 0),
    status text NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'fulfilled', 'cancelled')),
    fulfiller_account bigint,
    fulfiller_name text,
    created_at timestamptz NOT NULL DEFAULT now(),
    closed_at timestamptz
);
CREATE INDEX requests_open_idx ON requests (status) WHERE closed_at IS NULL;
-- One open request per pilot per blueprint.
CREATE UNIQUE INDEX requests_one_open_idx ON requests (item_id, requester_account)
    WHERE closed_at IS NULL;
