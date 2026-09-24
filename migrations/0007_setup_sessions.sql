-- First-run wizard: a browser that entered the setup token. Only the
-- SHA-256 of the cookie is stored. Unlocking again replaces any previous
-- session, and claiming ownership ends it.
CREATE TABLE core.setup_sessions (
    token_hash bytea PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
