-- Latest known corporation/alliance per character, from ESI's public bulk
-- affiliation endpoint.
ALTER TABLE core.characters
    ADD COLUMN corporation_id bigint,
    ADD COLUMN alliance_id bigint,
    ADD COLUMN affiliation_checked_at timestamptz;

-- Access tier, derived from the main's affiliation.
ALTER TABLE core.accounts
    ADD COLUMN tier text NOT NULL DEFAULT 'guest'
        CHECK (tier IN ('member', 'allied', 'guest')),
    ADD COLUMN tier_evaluated_at timestamptz;

-- Which alliances and corporations make a main Member or Allied.
-- EVE ids are unique across entity types.
CREATE TABLE core.tier_rules (
    entity_id bigint PRIMARY KEY,
    entity_kind text NOT NULL CHECK (entity_kind IN ('alliance', 'corporation')),
    tier text NOT NULL CHECK (tier IN ('member', 'allied')),
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
