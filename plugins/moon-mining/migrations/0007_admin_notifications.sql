-- aa-moonmining's MOONMINING_ADMIN_NOTIFICATIONS_ENABLED (on there):
-- superusers and holders of `manage` hear when an owner is added and when
-- ESI refuses one's refineries. On for a new install; an install already
-- in use keeps none until a manager turns them on in Settings.
ALTER TABLE settings
    ADD COLUMN admin_notifications boolean NOT NULL DEFAULT false,
    -- Whether the owners in use when this arrived are recorded (as told
    -- already), so turning the notices on announces only owners added
    -- after.
    ADD COLUMN sources_known boolean NOT NULL DEFAULT false;
UPDATE settings SET admin_notifications = true, sources_known = true
WHERE NOT EXISTS (SELECT 1 FROM structures)
  AND NOT EXISTS (SELECT 1 FROM extractions)
  AND NOT EXISTS (SELECT 1 FROM observers)
  AND NOT EXISTS (SELECT 1 FROM surveys)
  AND NOT EXISTS (SELECT 1 FROM cadences)
  AND NOT EXISTS (SELECT 1 FROM corporations);

-- The owners (data sources) seen, so each one added is announced once. A
-- row stays when its source goes: the host's list can come back empty on
-- a fault, and dropping rows would announce everyone again. A character
-- added for another corporation is a new owner.
CREATE TABLE sources (
    character_id bigint NOT NULL,
    corporation_id bigint NOT NULL,
    character_name text NOT NULL,
    seen_at timestamptz NOT NULL DEFAULT now(),
    announced boolean NOT NULL DEFAULT false,
    -- Since when ESI refuses its refineries (403): told once, cleared by
    -- a read that works.
    failing_since timestamptz,
    PRIMARY KEY (character_id, corporation_id)
);
