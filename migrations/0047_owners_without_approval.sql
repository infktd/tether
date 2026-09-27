-- App owners (data sources) are added as Alliance Auth adds them: a holder
-- of the app's add permission logs in with the character, and it's in use
-- at once, with no admin approval (Jay, 2026-09-26). Offers still waiting
-- for an admin become owners now, for the corporation their character is
-- in, audited as the system's doing. One whose corporation isn't known
-- stays unused until it's added again.
WITH approved AS (
    UPDATE core.plugin_data_sources d
    SET approved_at = now(), corporation_id = c.corporation_id
    FROM core.characters c
    WHERE c.id = d.character_id
      AND d.approved_at IS NULL
      AND c.corporation_id IS NOT NULL
    RETURNING d.plugin_id, d.character_id
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'plugin.data_source_approved', 'plugin:' || plugin_id,
       jsonb_build_object(
           'character_id', character_id,
           'reason', 'owners need no admin approval (Alliance Auth style)'
       )
FROM approved;
