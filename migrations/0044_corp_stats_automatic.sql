-- Corporation Stats reads every covered corporation's member list itself,
-- as Alliance Auth's does: Member requires the member list scope of every
-- character, and the daily job reads each list with any registered Member
-- character in that corporation. Nobody offers a source and no admin
-- approves one any more.

-- The character whose token last read the list: tried first next time.
ALTER TABLE core.corp_member_lists
    ADD COLUMN source_character_id bigint REFERENCES core.characters (id) ON DELETE SET NULL;

-- Offered and approved sources are gone, with the logins that offered one.
DROP TABLE core.corp_sources;
DELETE FROM core.login_attempts WHERE purpose = 'corp_source';
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'data_source', 'change_main', 'reauth'));

-- Member requires one more scope now: every account's compliance is
-- evaluated again at once, not at the next scheduled sync.
INSERT INTO core.jobs (kind)
SELECT 'states.evaluate_all' WHERE EXISTS (SELECT 1 FROM core.accounts);
