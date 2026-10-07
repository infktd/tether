-- aa-structures' last_online_at: when a sync last saw any of an Upwell
-- structure's services online. Out of fuel, a structure is in Low power
-- for 7 days after, then Abandoned; never seen online, "Abandoned?"
-- (unless it's anchoring). Unknown until the next sync.
ALTER TABLE structures ADD COLUMN last_online timestamptz;
