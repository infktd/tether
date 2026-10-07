-- Which read of its owner last saw each blueprint (a number for that
-- read). An owner's blueprints are stored a thousand at a time, so a
-- library of tens of thousands fits the host's limits on one call; those
-- the latest read didn't see go once all are stored.
ALTER TABLE blueprints ADD COLUMN seen bigint;
