-- Personal access tokens (F19): for bots and scripts calling the JSON API.
-- Stored hashed; scopes are permission names (and `account:read`), and a
-- token never does more than its account may at the time of use.
CREATE TABLE core.personal_tokens (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    token_hash bytea NOT NULL UNIQUE,
    -- The first characters, to tell tokens apart.
    prefix text NOT NULL,
    scopes text[] NOT NULL CHECK (cardinality(scopes) BETWEEN 1 AND 100),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    last_used_at timestamptz,
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '366 days')
);
CREATE INDEX personal_tokens_account_idx ON core.personal_tokens (account_id);
