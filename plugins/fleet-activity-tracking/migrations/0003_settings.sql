-- aa-afat's Setting: one row, with aa-afat's defaults. A new link's
-- expiry unless the FC picks another; how long after expiring a link can
-- be reopened (once), and for how long; how many days logs are kept.
CREATE TABLE settings (
    id integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    expiry_minutes integer NOT NULL DEFAULT 60 CHECK (expiry_minutes BETWEEN 1 AND 1440),
    reopen_grace_minutes integer NOT NULL DEFAULT 60
        CHECK (reopen_grace_minutes BETWEEN 0 AND 1440),
    reopen_duration_minutes integer NOT NULL DEFAULT 60
        CHECK (reopen_duration_minutes BETWEEN 1 AND 1440),
    log_days integer NOT NULL DEFAULT 60 CHECK (log_days BETWEEN 1 AND 3650)
);
INSERT INTO settings DEFAULT VALUES;
