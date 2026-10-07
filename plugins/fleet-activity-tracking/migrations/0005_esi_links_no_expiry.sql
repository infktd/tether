-- aa-afat's ESI FAT links have no expiry: they're tracked until the fleet
-- ends. Such a link has no expires_at while it tracks; when tracking stops
-- it's set to that moment, which closes the link.
ALTER TABLE links ALTER COLUMN expires_at DROP NOT NULL;
