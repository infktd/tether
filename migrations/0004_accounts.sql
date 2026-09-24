-- One account holds many characters; exactly one of them is the main.
CREATE TABLE core.accounts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    main_character_id bigint NOT NULL,
    is_owner boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- At most one owner, ever.
CREATE UNIQUE INDEX accounts_single_owner_idx ON core.accounts (is_owner) WHERE is_owner;

CREATE TABLE core.characters (
    id bigint PRIMARY KEY, -- EVE character id
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz,
    UNIQUE (account_id, id)
);
CREATE INDEX characters_account_idx ON core.characters (account_id);

-- The main must be one of the account's own characters. Deferred so an
-- account and its first character can be inserted in one transaction.
ALTER TABLE core.accounts
    ADD CONSTRAINT accounts_main_character_fk
    FOREIGN KEY (id, main_character_id) REFERENCES core.characters (account_id, id)
    DEFERRABLE INITIALLY DEFERRED;

-- Sessions now belong to accounts. Nothing is deployed yet, so dropping
-- existing (character-only) sessions loses nothing.
DELETE FROM core.sessions;
ALTER TABLE core.sessions
    DROP COLUMN character_id,
    DROP COLUMN character_name,
    ADD COLUMN account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE;
CREATE INDEX sessions_account_idx ON core.sessions (account_id);
