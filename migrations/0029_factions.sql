-- States cover factions (AA's Member Factions): a character's faction
-- warfare militia, from ESI's affiliation endpoint, and states may list
-- factions beside alliances, corporations and characters.
ALTER TABLE core.characters ADD COLUMN faction_id bigint;

ALTER TABLE core.state_entities DROP CONSTRAINT state_entities_entity_kind_check;
ALTER TABLE core.state_entities ADD CONSTRAINT state_entities_entity_kind_check
    CHECK (entity_kind IN ('alliance', 'corporation', 'character', 'faction'));
