-- aa-memberaudit's settings, on the app's Settings page instead of the
-- server's configuration: how long mail, contracts and wallet history are
-- kept (MEMBERAUDIT_DATA_RETENTION_LIMIT, at least 7 days), how many mails
-- are kept per character (MEMBERAUDIT_MAX_MAILS), whether corporation
-- roles are read (MEMBERAUDIT_FEATURE_ROLES_ENABLED, off by default) and
-- how long a character stays shared (MEMBERAUDIT_SHARING_TIMEOUT, in
-- minutes; 0 keeps it shared until its pilot stops). AA's defaults.
CREATE TABLE settings (
    id int PRIMARY KEY CHECK (id = 1),
    retention_days int NOT NULL DEFAULT 360 CHECK (retention_days BETWEEN 7 AND 3650),
    max_mails int NOT NULL DEFAULT 250 CHECK (max_mails BETWEEN 1 AND 5000),
    roles_enabled boolean NOT NULL DEFAULT false,
    sharing_timeout_minutes int NOT NULL DEFAULT 0
        CHECK (sharing_timeout_minutes BETWEEN 0 AND 525600)
);
INSERT INTO settings (id) VALUES (1);

-- Roles are off by default, as in aa-memberaudit: what was read goes.
DELETE FROM roles;
DELETE FROM section_syncs WHERE section = 'roles';

-- Characters their pilots share (aa-memberaudit's is_shared): holders of
-- view_shared_characters may open their sheets. A share counts only while
-- the character's owner's main is the one who shared it, so a character
-- sold on isn't shared on its new pilot's behalf.
ALTER TABLE characters ADD COLUMN is_shared boolean NOT NULL DEFAULT false;
ALTER TABLE characters ADD COLUMN shared_at timestamptz;
ALTER TABLE characters ADD COLUMN shared_by_main bigint;
