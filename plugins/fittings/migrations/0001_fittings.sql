-- Fittings' own schema (the plugin's; the host runs this once).
-- allianceauth-fittings' Fitting, FittingItem, Doctrine and Category, with
-- the item types looked up from ESI kept here instead of an SDE.

-- Item types: the name from ESI's /universe/ids when a fit is added, then
-- the item group and required skills from /universe/types in the
-- background (looked_up_at stays null until then).
CREATE TABLE types (
    type_id bigint PRIMARY KEY,
    name text NOT NULL,
    group_id bigint,
    -- Required skills as [[skill type id, level], ...].
    skills jsonb,
    looked_up_at timestamptz
);
CREATE INDEX types_name_idx ON types (lower(name));

-- Item groups (Frigate, Drone, ...) and their categories.
CREATE TABLE item_groups (
    group_id bigint PRIMARY KEY,
    category_id bigint NOT NULL,
    name text NOT NULL
);

-- AA's Fitting.
CREATE TABLE fits (
    id bigserial PRIMARY KEY,
    name text NOT NULL,
    hull_type_id bigint NOT NULL,
    -- Tether's addition: the fit's role in its doctrines (DPS, Logistics).
    role text NOT NULL DEFAULT '',
    description text NOT NULL DEFAULT '',
    -- The EFT text as pasted (line breaks normalised).
    eft text NOT NULL,
    created_by_character_id bigint NOT NULL,
    created_by_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- AA's unique_together (ship type, name).
    UNIQUE (hull_type_id, name)
);

-- AA's FittingItem: one row per EFT line that names an item, in the order
-- pasted. `slot` is where the EFT's section order puts it: low, mid, high,
-- rig, subsystem, service, bay (an "x5" section: drones, fighters or
-- cargo) or other (a section after the bays: implants and boosters). The
-- item's category decides the last few once it's known.
CREATE TABLE fit_items (
    fit_id bigint NOT NULL REFERENCES fits ON DELETE CASCADE,
    position integer NOT NULL,
    slot text NOT NULL
        CHECK (slot IN ('low', 'mid', 'high', 'rig', 'subsystem', 'service', 'bay', 'other')),
    type_id bigint NOT NULL,
    charge_type_id bigint,
    quantity integer NOT NULL DEFAULT 1 CHECK (quantity > 0),
    offline boolean NOT NULL DEFAULT false,
    PRIMARY KEY (fit_id, position)
);
CREATE INDEX fit_items_type_idx ON fit_items (type_id);

-- AA's Doctrine. Its icon is one of the fits' hulls (AA's icon_url); none
-- means its main hull, the one most of its fits are.
CREATE TABLE doctrines (
    id bigserial PRIMARY KEY,
    name text NOT NULL,
    description text NOT NULL DEFAULT '',
    icon_type_id bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE doctrine_fits (
    doctrine_id bigint NOT NULL REFERENCES doctrines ON DELETE CASCADE,
    fit_id bigint NOT NULL REFERENCES fits ON DELETE CASCADE,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (doctrine_id, fit_id)
);
CREATE INDEX doctrine_fits_fit_idx ON doctrine_fits (fit_id);

-- AA's Category: a tag on fits and doctrines (a doctrine's fits count as
-- in its categories). A category with groups is seen, with what's in it,
-- only by members of those groups (Tether's group ids); one without is
-- public.
CREATE TABLE categories (
    id bigserial PRIMARY KEY,
    name text NOT NULL,
    color text NOT NULL DEFAULT '#FFFFFF',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE category_fits (
    category_id bigint NOT NULL REFERENCES categories ON DELETE CASCADE,
    fit_id bigint NOT NULL REFERENCES fits ON DELETE CASCADE,
    PRIMARY KEY (category_id, fit_id)
);
CREATE INDEX category_fits_fit_idx ON category_fits (fit_id);

CREATE TABLE category_doctrines (
    category_id bigint NOT NULL REFERENCES categories ON DELETE CASCADE,
    doctrine_id bigint NOT NULL REFERENCES doctrines ON DELETE CASCADE,
    PRIMARY KEY (category_id, doctrine_id)
);
CREATE INDEX category_doctrines_doctrine_idx ON category_doctrines (doctrine_id);

CREATE TABLE category_groups (
    category_id bigint NOT NULL REFERENCES categories ON DELETE CASCADE,
    group_id bigint NOT NULL,
    PRIMARY KEY (category_id, group_id)
);

-- Every category a fit is in: its own, and its doctrines'.
CREATE VIEW fit_categories AS
    SELECT fit_id, category_id FROM category_fits
    UNION
    SELECT df.fit_id, cd.category_id FROM doctrine_fits df
    JOIN category_doctrines cd ON cd.doctrine_id = df.doctrine_id;
