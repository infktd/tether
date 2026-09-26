-- Where a signed-out visitor was headed: a page that sent them to log in
-- records its path here, keyed by the SHA-256 of a random browser token
-- (the __Host-tether_next cookie), and /auth/login moves it onto the login
-- attempt, so the browser lands back on that page afterwards. Only the
-- token's hash is stored; the path never leaves the server.
CREATE TABLE core.login_destinations (
    browser_hash bytea PRIMARY KEY,
    path text NOT NULL CHECK (length(path) BETWEEN 1 AND 512),
    expires_at timestamptz NOT NULL
);
CREATE INDEX login_destinations_expires_idx ON core.login_destinations (expires_at);

-- Sudo mode: owner-only and sensitive admin actions need an EVE SSO login
-- with the account's main in the last few minutes. A session records when
-- that last happened (NULL: not since it began, as for every session from
-- before this migration), and a login attempt can be a re-authentication,
-- naming the action it was for (for the audit log).
ALTER TABLE core.sessions ADD COLUMN reauthenticated_at timestamptz;
ALTER TABLE core.login_attempts ADD COLUMN reauth_action text;
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'data_source', 'corp_source', 'change_main', 'reauth'));
