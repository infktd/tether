-- Apps read characters as Alliance Auth's do (K3, Jay 2026-09-27): whoever
-- holds one of an app's permissions, whatever their state, may register
-- characters for it, and the app reads the characters whose token carries
-- every one of its user scopes. Member no longer requires every installed
-- app's user scopes by itself; admins require scopes per state, as AA's
-- Member Audit compliance groups do.
--
-- Nobody's compliance changes on upgrade: the app scopes Member required
-- until now become Member's own requirements (as if an admin had added
-- them), recorded in the audit log by the system. An admin can drop them
-- on the States page.
WITH added AS (
    INSERT INTO core.state_scopes (state_id, scope, added_by)
    SELECT s.id, scope, NULL
    FROM core.states s
    CROSS JOIN core.plugins p
    CROSS JOIN unnest(p.user_scopes) AS scope
    WHERE s.builtin = 'member'
    ON CONFLICT DO NOTHING
    RETURNING state_id, scope
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'state.scope_add', 'state:' || a.state_id,
       jsonb_build_object(
           'state', s.name,
           'scopes', jsonb_agg(a.scope ORDER BY a.scope),
           'reason', 'upgrade: apps no longer make Member require their scopes, so the ones it required are kept as its own'
       )
FROM added a
JOIN core.states s ON s.id = a.state_id
GROUP BY a.state_id, s.name;

-- Whether an account holds any of an app's permissions, as the host's
-- effective permissions work them out (crates/db/src/permissions.rs,
-- `held_in`): nothing for a deactivated or blacklisted account; every
-- app for superusers (even one that adds no permissions); otherwise the
-- grants to its state, to its groups while it has a main, and to the
-- account itself (AA's user permissions, main or not). An app's
-- characters are on such accounts.
CREATE FUNCTION core.holds_app_permission(for_account bigint, app_id text) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT COALESCE((
        SELECT a.active AND NOT core.blacklisted(a.id) AND (
            a.is_owner
            OR EXISTS (
                SELECT 1 FROM core.permission_grants g
                JOIN core.plugin_permissions pp ON pp.permission = g.permission
                WHERE pp.plugin_id = app_id
                  AND (g.state_id = a.state_id
                       OR (a.main_character_id IS NOT NULL
                           AND g.group_id IN (SELECT m.group_id FROM core.group_members m
                                              WHERE m.account_id = a.id))
                       OR g.account_id = a.id)))
        FROM core.accounts a WHERE a.id = for_account), false)
$$;
