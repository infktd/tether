-- When a character's skills, and its assets, were last stored whole: the
-- Secure Groups filters answer only for characters with complete data
-- (a first sync that failed, or assets cut short, must not read as "has
-- none", or a reversed filter would let the account in).
ALTER TABLE characters ADD COLUMN skills_at timestamptz;
ALTER TABLE characters ADD COLUMN assets_at timestamptz;
