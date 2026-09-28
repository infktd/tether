-- aa-timezones' panels: a name and an IANA time zone each. None set: its
-- ten default panels.
CREATE TABLE panels (
    id serial PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (char_length(name) BETWEEN 1 AND 60),
    zone text NOT NULL CHECK (char_length(zone) BETWEEN 1 AND 60)
);

-- A pilot's own time zone (aa-timezones shows the browser's local time;
-- Tether can't see it, so the pilot picks it once).
CREATE TABLE viewer_zones (
    account_id bigint PRIMARY KEY,
    zone text NOT NULL CHECK (char_length(zone) BETWEEN 1 AND 60)
);
