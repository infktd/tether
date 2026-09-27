-- Apps read characters as Alliance Auth's do (K3, Jay 2026-09-27): whoever
-- holds one of an app's permissions, whatever their state, registers
-- characters for it (as aa-memberaudit's Register Character), and the app
-- reads exactly the characters registered for it, while their account
-- holds one of its permissions and their token carries every one of its
-- user scopes. Member no longer requires every installed app's user scopes
-- by itself; admins require scopes per state, as AA's Member Audit
-- compliance groups do.
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

-- The characters registered for each app, and by whom (NULL: the system,
-- at this upgrade). A character leaves every app when it leaves its
-- account (removed, sold, moved: a deleted character's rows go with it,
-- and a moved one's are dropped below), and an app's with the app.
CREATE TABLE core.app_characters (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    character_id bigint NOT NULL REFERENCES core.characters (id) ON DELETE CASCADE,
    registered_at timestamptz NOT NULL DEFAULT now(),
    registered_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    PRIMARY KEY (plugin_id, character_id)
);
CREATE INDEX app_characters_character_idx ON core.app_characters (character_id);

CREATE FUNCTION core.app_characters_follow_account() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM core.app_characters WHERE character_id = NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER characters_leave_apps AFTER UPDATE OF account_id ON core.characters
    FOR EACH ROW WHEN (OLD.account_id IS DISTINCT FROM NEW.account_id)
    EXECUTE FUNCTION core.app_characters_follow_account();

-- Registering for an app is its own login purpose (the app in plugin_id).
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'register_app', 'data_source', 'change_main', 'reauth'));

-- Nothing changes on upgrade: every character an app could read until now
-- (a Member account's, its token carrying all the app's user scopes) is
-- registered for it, by the system, audited per app. A revoked token counts
-- with the scopes it carried: it is read again once its pilot logs in, as
-- before.
WITH registered AS (
    INSERT INTO core.app_characters (plugin_id, character_id, registered_by)
    SELECT p.id, c.id, NULL
    FROM core.plugins p
    JOIN core.characters c ON true
    JOIN core.accounts a ON a.id = c.account_id
    JOIN core.states s ON s.id = a.state_id
    JOIN core.character_tokens t ON t.character_id = c.id
    WHERE cardinality(p.user_scopes) > 0
      AND s.builtin = 'member'
      AND t.scopes @> p.user_scopes
    ON CONFLICT DO NOTHING
    RETURNING plugin_id, character_id
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'plugin.characters_registered', 'plugin:' || plugin_id,
       jsonb_build_object(
           'characters', count(*),
           'reason', 'upgrade: apps now read only the characters registered for them, so those they read until now were registered'
       )
FROM registered
GROUP BY plugin_id;
