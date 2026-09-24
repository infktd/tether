-- CCP's owner hash from the verified SSO token. It changes when a
-- character moves to another EVE account (sold or transferred), which
-- must unlink it from whoever had it before. NULL until the character
-- next logs in.
ALTER TABLE core.characters ADD COLUMN owner_hash text;
