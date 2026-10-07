-- When each alliance and corporation was last tried, read or not: an
-- update reads the longest untried first, and leaves what its ESI calls
-- don't reach to a follow-up run.
ALTER TABLE tracked ADD COLUMN attempted_at timestamptz;
