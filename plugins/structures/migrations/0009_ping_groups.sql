-- aa-structures' ping groups: an owner's and a channel's (its webhook's)
-- groups, whose Discord roles every message of that owner to that channel
-- pings, by group name.
ALTER TABLE owner_settings ADD COLUMN ping_groups text[];

CREATE TABLE channel_ping_groups (
    channel text PRIMARY KEY,
    groups text[] NOT NULL
);

-- The groups a waiting message pings, decided when it was queued.
ALTER TABLE outbox ADD COLUMN ping_groups text[];
