-- AA's permissions and settings for Fleet Pings, the Blacklist and Secure
-- Groups (Jay, 2026-09-27: "operate like they do").

-- Fleet Pings: aa-fleetpings' fleetpings.basic_access replaces fleet.ping.
-- Grants and personal access tokens' scopes move to the new name.
UPDATE core.permission_grants SET permission = 'fleetpings.basic_access'
WHERE permission = 'fleet.ping';
UPDATE core.personal_tokens
SET scopes = array_replace(scopes, 'fleet.ping', 'fleetpings.basic_access')
WHERE 'fleet.ping' = ANY (scopes);

-- The Blacklist, as allianceauth-blacklist: one kind of note (AA's
-- EveNote) on a pilot, corporation or alliance, with a reason, flagged
-- blacklisted, restricted or ultra restricted, and comments (with the same
-- tiers). A pilot's note keeps their corporation and alliance, for the
-- own-corporation permissions. The Blacklist is the blacklisted notes.
ALTER TABLE core.pilot_notes
    ADD COLUMN blacklisted boolean NOT NULL DEFAULT false,
    ADD COLUMN restricted boolean NOT NULL DEFAULT false,
    ADD COLUMN ultra_restricted boolean NOT NULL DEFAULT false,
    ADD COLUMN corporation_id bigint,
    ADD COLUMN corporation_name text,
    ADD COLUMN alliance_id bigint,
    ADD COLUMN alliance_name text,
    ADD COLUMN edited_at timestamptz;
INSERT INTO core.pilot_notes
    (entity_id, entity_kind, name, note, added_by, added_by_name, added_at, blacklisted)
SELECT entity_id, entity_kind, name, reason, added_by, added_by_name, added_at, true
FROM core.blacklist;
DROP TABLE core.blacklist;
UPDATE core.pilot_notes n SET corporation_id = c.corporation_id, alliance_id = c.alliance_id
FROM core.characters c
WHERE n.entity_kind = 'character' AND c.id = n.entity_id;
UPDATE core.pilot_notes SET corporation_id = entity_id, corporation_name = name
WHERE entity_kind = 'corporation';
UPDATE core.pilot_notes SET alliance_id = entity_id, alliance_name = name
WHERE entity_kind = 'alliance';
UPDATE core.pilot_notes n SET corporation_name = e.name
FROM core.entity_names e WHERE n.corporation_name IS NULL AND e.id = n.corporation_id;
UPDATE core.pilot_notes n SET alliance_name = e.name
FROM core.entity_names e WHERE n.alliance_name IS NULL AND e.id = n.alliance_id;
CREATE INDEX pilot_notes_blacklisted_idx ON core.pilot_notes (entity_id) WHERE blacklisted;
CREATE INDEX pilot_notes_corporation_idx ON core.pilot_notes (corporation_id);

CREATE TABLE core.pilot_note_comments (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    note_id bigint NOT NULL REFERENCES core.pilot_notes (id) ON DELETE CASCADE,
    comment text NOT NULL CHECK (length(comment) BETWEEN 1 AND 2000),
    restricted boolean NOT NULL DEFAULT false,
    ultra_restricted boolean NOT NULL DEFAULT false,
    added_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    added_by_name text NOT NULL,
    added_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pilot_note_comments_note_idx ON core.pilot_note_comments (note_id, added_at);

-- As AA, blacklisting goes by the main: the Blacklist state's member
-- characters, corporations and alliances are the blacklisted notes', and a
-- state comes from the main. Never the owner.
CREATE OR REPLACE FUNCTION core.blacklisted(account bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT COALESCE((
        SELECT NOT a.is_owner AND EXISTS (
            SELECT 1 FROM core.characters c
            JOIN core.pilot_notes n
              ON n.blacklisted AND n.entity_id IN (c.id, c.corporation_id, c.alliance_id)
            WHERE c.id = a.main_character_id)
        FROM core.accounts a WHERE a.id = account), false)
$$;

-- allianceauth-blacklist's 16 permissions replace Tether's three; each
-- grant (and personal access token scope) of an old one becomes the new
-- ones that do what it did.
CREATE TEMP TABLE blacklist_renames (old text, new text) ON COMMIT DROP;
INSERT INTO blacklist_renames VALUES
    ('blacklist.view_blacklist', 'blacklist.view_eve_blacklist'),
    ('blacklist.view_blacklist', 'blacklist.view_eve_notes'),
    ('blacklist.view_blacklist', 'blacklist.view_eve_note_comments'),
    ('blacklist.add_notes', 'blacklist.add_new_eve_notes'),
    ('blacklist.add_notes', 'blacklist.add_new_eve_note_comments'),
    ('blacklist.manage_blacklist', 'blacklist.add_new_eve_notes'),
    ('blacklist.manage_blacklist', 'blacklist.add_to_blacklist');
INSERT INTO core.permission_grants (permission, state_id, group_id, account_id)
SELECT r.new, g.state_id, g.group_id, g.account_id
FROM core.permission_grants g JOIN blacklist_renames r ON r.old = g.permission
ON CONFLICT DO NOTHING;
DELETE FROM core.permission_grants WHERE permission IN (SELECT old FROM blacklist_renames);
UPDATE core.personal_tokens t SET scopes = ARRAY(
    SELECT DISTINCT COALESCE(r.new, s.scope)
    FROM unnest(t.scopes) AS s(scope) LEFT JOIN blacklist_renames r ON r.old = s.scope)
WHERE t.scopes && ARRAY(SELECT old FROM blacklist_renames);
