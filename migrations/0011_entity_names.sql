-- Names of EVE entities (alliances, corporations, characters...) from
-- ESI, cached because they're looked up constantly and rarely change.
CREATE TABLE core.entity_names (
    id bigint PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL,
    fetched_at timestamptz NOT NULL DEFAULT now()
);
