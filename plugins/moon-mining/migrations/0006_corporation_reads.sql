-- When each corporation was last read: its extractions and refineries by
-- the sync (synced_at), its mining observers listed by the hourly ledger
-- run (listed_at). Each run reads the longest unread first, and one out of
-- ESI calls carries on a minute later, so every corporation gets its turn
-- however many there are. A corporation not here, or with no time, hasn't
-- been read yet.
CREATE TABLE corporations (
    corporation_id bigint PRIMARY KEY,
    synced_at timestamptz,
    listed_at timestamptz
);
