-- When a refinery was last missing from its corporation's structures
-- (lost, unanchored or handed to a corporation not read): it no longer
-- owns its moon and the planner leaves it out, as aa-moonmining deletes
-- it. Kept, not deleted, so its past extractions keep their refinery's
-- name and system. Cleared if ESI lists it again.
ALTER TABLE structures ADD COLUMN gone_at timestamptz;
