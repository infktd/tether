-- Fleet Pings, as aa-fleetpings: a ping carries fleet details (drawn as a
-- Discord embed and as copy-paste text), admins keep lists of fleet types,
-- doctrines, formup locations and comms, and channels, targets, fleet types
-- and doctrines can be limited to states or groups.

ALTER TABLE core.fleet_pings
    ADD COLUMN pre_ping boolean NOT NULL DEFAULT false,
    ADD COLUMN fleet_type text,
    -- The fleet type's embed colour when sent, `#rrggbb`.
    ADD COLUMN embed_color text CHECK (embed_color ~ '^#[0-9a-f]{6}$'),
    ADD COLUMN fc_name text,
    ADD COLUMN fleet_name text,
    ADD COLUMN formup_location text,
    -- NULL with formup_now false: not said.
    ADD COLUMN formup_time timestamptz,
    ADD COLUMN formup_now boolean NOT NULL DEFAULT false,
    ADD COLUMN comms text,
    ADD COLUMN doctrine text,
    ADD COLUMN doctrine_link text,
    -- NULL: not said.
    ADD COLUMN srp boolean;

-- What the ping form offers. Free text is allowed too, as aa-fleetpings.
CREATE TABLE core.ping_options (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('fleet_type', 'doctrine', 'formup', 'comms')),
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    -- Doctrines: a page about it (https only).
    link text CHECK (link IS NULL OR (link ~ '^https://' AND length(link) <= 500)),
    -- Fleet types: the embed colour, `#rrggbb`.
    color text CHECK (color IS NULL OR color ~ '^#[0-9a-f]{6}$'),
    added_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (kind, name)
);

-- Who may use a channel, target, fleet type or doctrine. An item with no
-- rows is for everyone who may ping; with rows, only for accounts in one
-- of the states or groups listed. `item` is `channel:<id>`, `here`,
-- `everyone`, `role:<id>` or `option:<id>`.
CREATE TABLE core.ping_restrictions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    item text NOT NULL CHECK (item ~ '^(channel:[0-9]+|here|everyone|role:[0-9]+|option:[0-9]+)$'),
    state_id bigint REFERENCES core.states (id) ON DELETE CASCADE,
    group_id bigint REFERENCES core.groups (id) ON DELETE CASCADE,
    added_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((state_id IS NULL) <> (group_id IS NULL)),
    UNIQUE NULLS NOT DISTINCT (item, state_id, group_id)
);
CREATE INDEX ping_restrictions_item_idx ON core.ping_restrictions (item);
