-- aa-structures' STRUCTURES_TIMERS_ARE_CORP_RESTRICTED: the timers
-- Structures publishes for Structure Timers are seen only by the owning
-- corporation. Off by default, as there.
ALTER TABLE settings ADD COLUMN timers_corporation_only boolean NOT NULL DEFAULT false;
