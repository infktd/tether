-- Discord access starts as Alliance Auth's does: "Can access the Discord
-- service" (discord.access_discord) granted to nobody. AA's migrations
-- grant no permission, and its docs have admins add it to Member, and to
-- Blue if they want; 0027 granted it to every state but Guest. The Discord
-- page says so until someone holds it.
--
-- Only a new instance changes, before anyone has signed in. An instance
-- with accounts keeps its grants as they are, so nobody linked leaves the
-- server.
DELETE FROM core.permission_grants
WHERE permission = 'discord.access_discord'
  AND NOT EXISTS (SELECT 1 FROM core.accounts);

-- Nobody can be linked yet: the checks those deletes queued (0027's
-- trigger) have nothing to do.
DELETE FROM core.jobs
WHERE kind = 'discord.sync_all' AND payload = '{}'::jsonb
  AND NOT EXISTS (SELECT 1 FROM core.accounts);
