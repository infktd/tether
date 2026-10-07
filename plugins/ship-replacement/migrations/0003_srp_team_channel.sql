-- aa-srp's Setting: the SRP team's Discord channel, where new requests are
-- posted (srp_team_discord_channel_id). None by default, as aa-srp's.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    channel text
);
INSERT INTO settings (id) VALUES (1);

-- New requests' cards for that channel, queued with the request and sent
-- by the relay job; sent ones are kept a week. A request removed with its
-- fleet takes its card along.
CREATE TABLE outbox (
    id serial PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES requests ON DELETE CASCADE,
    channel text NOT NULL,
    queued_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    failed text
);
