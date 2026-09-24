-- EVE SSO refresh tokens, encrypted with ENCRYPTION_KEY (never stored
-- here). Access tokens are only ever held in memory.
CREATE TABLE core.character_tokens (
    character_id bigint PRIMARY KEY REFERENCES core.characters (id) ON DELETE CASCADE,
    refresh_token bytea NOT NULL,
    scopes text[] NOT NULL DEFAULT '{}',
    state text NOT NULL DEFAULT 'valid' CHECK (state IN ('valid', 'revoked')),
    revoked_at timestamptz,
    revoked_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_refreshed_at timestamptz
);
