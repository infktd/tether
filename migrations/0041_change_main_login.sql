-- Change Main, as Alliance Auth's: besides picking a character already on
-- the account, the owner can log in with EVE SSO and make that character
-- the main (added to the account first, or moved from another one, as Add
-- Character does).
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'data_source', 'corp_source', 'change_main'));
