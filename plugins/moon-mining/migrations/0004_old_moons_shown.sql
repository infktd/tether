-- How many old moons the Extractions page lists beside the fresh ones
-- (the newest first); 0 hides them there.
ALTER TABLE settings
    ADD COLUMN old_moons_shown integer NOT NULL DEFAULT 5
        CHECK (old_moons_shown BETWEEN 0 AND 50);
