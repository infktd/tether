-- ESI's routes as the last check found them (aa-esi-status' latest status).
CREATE TABLE routes (
    method text NOT NULL,
    path text NOT NULL,
    status text NOT NULL,
    PRIMARY KEY (method, path)
);

-- Each check's counts, kept for 24 hours (aa-esi-status' history).
CREATE TABLE checks (
    checked_at timestamptz PRIMARY KEY,
    compatibility_date text NOT NULL DEFAULT '',
    ok integer NOT NULL,
    degraded integer NOT NULL,
    down integer NOT NULL,
    recovering integer NOT NULL,
    unknown integer NOT NULL
);
