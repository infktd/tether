-- AA's permissions and settings for Fleet Pings, the Blacklist and Secure
-- Groups (Jay, 2026-09-27: "operate like they do").

-- Fleet Pings: aa-fleetpings' fleetpings.basic_access replaces fleet.ping.
-- Grants and personal access tokens' scopes move to the new name.
UPDATE core.permission_grants SET permission = 'fleetpings.basic_access'
WHERE permission = 'fleet.ping';
UPDATE core.personal_tokens
SET scopes = array_replace(scopes, 'fleet.ping', 'fleetpings.basic_access')
WHERE 'fleet.ping' = ANY (scopes);
