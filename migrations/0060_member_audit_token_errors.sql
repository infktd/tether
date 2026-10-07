-- aa-memberaudit's token-error notice (MEMBERAUDIT_NOTIFY_TOKEN_ERRORS): a
-- character registered with Member Audit whose EVE login stopped working,
-- or lacks one of Member Audit's scopes, tells its pilot once, until it
-- works again. When it was told (AA's Character.token_error_notified_at),
-- one per registration: it goes with the registration.
ALTER TABLE core.app_characters ADD COLUMN token_error_notified_at timestamptz;

-- On unless set, as in AA, so a new instance sends it. An instance that
-- already has accounts keeps what it did (no notice) until an admin turns
-- it on under Settings, Notifications.
INSERT INTO core.settings (key, value)
SELECT 'notifications.member_audit_token_errors', 'false'::jsonb
WHERE EXISTS (SELECT 1 FROM core.accounts)
ON CONFLICT (key) DO NOTHING;
