-- aa-structures' ping groups: an owner's and a channel's (its webhook's)
-- groups, whose Discord roles every message of that owner to that channel
-- pings. By the group's id, as aa-structures' are the group itself: a
-- renamed group is still pinged.
ALTER TABLE owner_settings ADD COLUMN ping_groups bigint[];

CREATE TABLE channel_ping_groups (
    channel text PRIMARY KEY,
    groups bigint[] NOT NULL
);

-- The name each group picked had when last saved, to say which one it was
-- once it's deleted or loses its Discord role.
CREATE TABLE ping_group_names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);

-- The groups a waiting message pings, decided when it was queued.
ALTER TABLE outbox ADD COLUMN ping_groups bigint[];
