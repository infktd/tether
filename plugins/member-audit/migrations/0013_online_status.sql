-- aa-memberaudit's online status (CharacterOnlineStatus): when the
-- character last logged in and out, and how many times it has, from ESI's
-- character-online (esi-location.read_online.v1).
ALTER TABLE characters ADD COLUMN last_login timestamptz;
ALTER TABLE characters ADD COLUMN last_logout timestamptz;
ALTER TABLE characters ADD COLUMN logins integer;
ALTER TABLE characters ADD COLUMN online boolean;
