-- When each route took its status, and every change since, for "since"
-- and the page's history. Routes already here keep no since: when they
-- took their status is unknown. What came before tracking began is too.
ALTER TABLE routes ADD COLUMN since timestamptz;

CREATE TABLE changes (
    at timestamptz NOT NULL,
    method text NOT NULL,
    path text NOT NULL,
    status text NOT NULL,
    was text NOT NULL
);

CREATE INDEX changes_at ON changes (at);

CREATE TABLE tracking (started timestamptz NOT NULL);
INSERT INTO tracking (started) VALUES (now());
