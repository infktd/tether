-- When each blueprint's place was last looked for, so one looked for and
-- not found reads "Unknown location" (aa-blueprints: "Location #<id>"),
-- not "Not read yet". A blueprint that moves is looked for again.
ALTER TABLE blueprints ADD COLUMN place_read_at timestamptz;
UPDATE blueprints SET place_read_at = now() WHERE place_id IS NOT NULL;

-- When each owner's places were last read: the oldest are read first, so
-- a run that runs out of ESI calls doesn't leave the same owners out
-- every time.
ALTER TABLE owners ADD COLUMN places_at timestamptz;

-- Offices and containers have ids in Upwell structures' range, and were
-- taken for structures: those places are forgotten, and read again (from
-- the corporation's assets) at the next places read. Blueprints straight
-- in a structure's deliveries were placed right, and stay.
UPDATE blueprints SET place_id = NULL, within = NULL, place_read_at = NULL
 WHERE owner_kind = 'corporation' AND place_id = location_id
   AND place_id > 1000000000000 AND location_flag <> 'CorpDeliveries';
-- Their names couldn't be read either: those nothing points to go.
DELETE FROM places p
 WHERE NOT p.named
   AND NOT EXISTS (SELECT 1 FROM blueprints b WHERE b.place_id = p.id);
