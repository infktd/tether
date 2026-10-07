-- When the sync last read each corporation's extractions and refineries.
-- A run reads the longest unread first, and one out of ESI calls carries
-- on a minute later, so every corporation gets its turn however many
-- there are. A corporation not here hasn't been read yet.
CREATE TABLE corporations (
    corporation_id bigint PRIMARY KEY,
    synced_at timestamptz NOT NULL
);
