-- Whether a place's name came from ESI: one that couldn't be read (the
-- owner's character may not dock there, say) is tried again within the
-- hour, through every owner with blueprints there, not left for a week.
ALTER TABLE places ADD COLUMN named boolean NOT NULL DEFAULT true;
UPDATE places SET named = false WHERE name LIKE 'Structure %' OR name LIKE 'Location %';
