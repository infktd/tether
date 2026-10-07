-- When each character's skills were last tried, read or not: a run reads
-- those not tried for a while, never read first, then the longest unread,
-- so one that can't be read doesn't keep the run going round.
ALTER TABLE skill_reads ADD COLUMN tried_at timestamptz;
