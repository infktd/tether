-- aa-moonmining's rules (Jay, 2026-09-27: "take AA's permissions and their
-- settings"): what it doesn't do is optional, and off unless chosen.
--
-- The Members-only window and the old-moon list for basic_access alone:
-- 0 hours is off, as aa-moonmining (extractions are for
-- extractions_access, and everyone else opens Moons). Off by default,
-- here too; a manager turns it on in Settings.
ALTER TABLE settings DROP CONSTRAINT IF EXISTS settings_fresh_hours_check;
ALTER TABLE settings
    ADD CONSTRAINT settings_fresh_hours_check CHECK (fresh_hours BETWEEN 0 AND 48),
    ALTER COLUMN fresh_hours SET DEFAULT 0,
    -- Pop pings: sent only once a manager picks a channel; off by
    -- default.
    ALTER COLUMN pings SET DEFAULT false;
UPDATE settings SET fresh_hours = 0;
UPDATE settings SET pings = false WHERE ping_channel IS NULL;
