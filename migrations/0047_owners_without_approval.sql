-- App owners (data sources) are added as Alliance Auth adds them: a holder
-- of the app's add permission logs in with one of their own characters,
-- and it's in use at once, with no admin approval (Jay, 2026-09-26).
-- Offers still waiting for an admin were made under the old rules, by
-- people who may not hold an add permission now, so they are dropped, not
-- approved: whoever may add owners adds them again in one login. Each is
-- recorded as removed by the system.
WITH removed AS (
    DELETE FROM core.plugin_data_sources
    WHERE approved_at IS NULL
    RETURNING plugin_id, character_id
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'plugin.data_source_removed', 'plugin:' || plugin_id,
       jsonb_build_object(
           'character_id', character_id,
           'reason', 'owners are now added, not offered'
       )
FROM removed;
