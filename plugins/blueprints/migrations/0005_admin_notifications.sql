-- aa-blueprints' BLUEPRINTS_ADMIN_NOTIFICATIONS_ENABLED (on there): admins
-- hear in Tether's notifications when a blueprint owner is added. On for
-- a new install; an install already in use sends none until a manager
-- turns them on, as before.
ALTER TABLE settings
    ADD COLUMN admin_notifications boolean NOT NULL DEFAULT false,
    ADD COLUMN sources_known boolean NOT NULL DEFAULT false;
UPDATE settings SET admin_notifications = true, sources_known = true
 WHERE NOT EXISTS (SELECT 1 FROM owners)
   AND NOT EXISTS (SELECT 1 FROM personal_owners)
   AND NOT EXISTS (SELECT 1 FROM blueprints);

-- The data sources seen (ids only), so each corporate owner added is
-- announced once. A row not seen for a week goes, with the week's grace
-- owners get, so a blip doesn't announce an owner again; an empty list,
-- which may be Tether not saying, forgets nothing while the app has
-- corporate owners. A character added for another corporation is a new
-- owner. Until sources_known, the sources seen are taken as announced:
-- an install in use announces only owners added after its first run.
CREATE TABLE sources (
    character_id bigint NOT NULL,
    corporation_id bigint NOT NULL,
    seen_at timestamptz NOT NULL DEFAULT now(),
    announced boolean NOT NULL DEFAULT false,
    PRIMARY KEY (character_id, corporation_id)
);
