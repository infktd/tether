-- Whether a refinery has a Moon Drill fitted (ESI lists it among its
-- services, online or not): the planner lists only drills. Unknown until
-- the next sync reads the structures.
ALTER TABLE structures ADD COLUMN drill boolean;
