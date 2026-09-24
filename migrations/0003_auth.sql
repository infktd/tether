-- Instance settings written by the first-run wizard (e.g. the SSO client id).
CREATE TABLE core.settings (
    key text PRIMARY KEY,
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- An SSO login between the redirect to CCP and the callback. Single use:
-- the callback deletes the row it consumes. browser_hash binds the attempt
-- to the browser that started it (login CSRF).
CREATE TABLE core.login_attempts (
    state text PRIMARY KEY,
    browser_hash bytea NOT NULL,
    pkce_verifier text NOT NULL,
    return_to text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX login_attempts_expires_idx ON core.login_attempts (expires_at);

-- Browser sessions. Only the SHA-256 of the cookie token is stored.
CREATE TABLE core.sessions (
    token_hash bytea PRIMARY KEY,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX sessions_expires_idx ON core.sessions (expires_at);
