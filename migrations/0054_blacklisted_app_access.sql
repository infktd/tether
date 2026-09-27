-- Whether an account may use an app, as core decides what it holds
-- (permissions::held_in): a blacklisted account holds what the Blacklist
-- state is granted, plus its own grants and its groups' while it has a
-- main. It was left out of every app here, which disagreed with what core
-- let it open (AA: blacklisting is the state, and that's all it changes).
CREATE OR REPLACE FUNCTION core.holds_app_permission(for_account bigint, app_id text) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT COALESCE((
        SELECT a.active AND (
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
