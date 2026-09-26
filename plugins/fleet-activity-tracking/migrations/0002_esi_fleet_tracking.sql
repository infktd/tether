-- aa-afat's ESI-tracked fleets: a FAT link may follow the fleet an FC's
-- character runs (an approved data source of the app), adding a FAT for
-- everyone in it while the link is open.
ALTER TABLE links
    ADD COLUMN esi_character_id bigint,
    ADD COLUMN esi_character_name text,
    -- NULL for a link members only click; else tracking or stopped.
    ADD COLUMN esi_state text CHECK (esi_state IN ('tracking', 'stopped')),
    -- Why tracking stopped: fleet_ended, not_boss, refused, data_source,
    -- token, cap, closed or manual.
    ADD COLUMN esi_stop_reason text,
    -- When tracking first started: the six-hour cap counts from here.
    ADD COLUMN esi_started_at timestamptz,
    ADD COLUMN esi_polled_at timestamptz;
CREATE INDEX links_tracking ON links (esi_polled_at) WHERE esi_state = 'tracking';
-- One tracking link per character: an FC runs one fleet at a time, and
-- one FC can't crowd out everyone else's polling.
CREATE UNIQUE INDEX links_one_tracking_per_character ON links (esi_character_id)
    WHERE esi_state = 'tracking';

-- What aa-afat records from an ESI fleet: the ship and the system.
ALTER TABLE fats
    ADD COLUMN ship_type_id bigint,
    ADD COLUMN system_id bigint,
    ADD COLUMN esi boolean NOT NULL DEFAULT false;
