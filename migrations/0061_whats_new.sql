-- What's new (crates/web-core/src/whats_new.rs): the newest CHANGELOG.md
-- release each account has seen, and since when, for the apps updated
-- since. NULL until the account's first page: then everything so far
-- counts as seen, so a new pilot doesn't get the history. Accounts here
-- already start before the first release, so they see it.
ALTER TABLE core.accounts ADD COLUMN whats_new_seen integer;
ALTER TABLE core.accounts ADD COLUMN whats_new_seen_at timestamptz;
UPDATE core.accounts SET whats_new_seen = 0, whats_new_seen_at = now();
