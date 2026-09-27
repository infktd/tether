-- A state can require an app, as Alliance Auth's Member Audit compliance
-- groups do: every character of its accounts registered for the app (and
-- its token carrying the app's scopes), not just the scopes. Plain scope
-- requirements (admin-added, and Member's core one) still check scopes.
CREATE TABLE core.state_apps (
    state_id bigint NOT NULL REFERENCES core.states (id) ON DELETE CASCADE,
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    added_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (state_id, plugin_id)
);

-- A state whose app scopes came from the app (an admin's "Require
-- <app>'s scopes", or the 0050 upgrade keeping what Member required of the
-- apps installed then), and still requires all of them, now requires the
-- app itself; those scope rows give way to it (the app's scopes are still
-- checked). Scopes admins added one by one stay plain requirements, even
-- if they happen to cover an app's.
WITH converted AS (
    INSERT INTO core.state_apps (state_id, plugin_id, added_by)
    SELECT s.id, p.id, NULL
    FROM core.states s
    CROSS JOIN core.plugins p
    WHERE cardinality(p.user_scopes) > 0
      AND p.user_scopes <@ ARRAY(SELECT x.scope FROM core.state_scopes x WHERE x.state_id = s.id)
      AND EXISTS (
        SELECT 1 FROM core.audit_log l
        WHERE l.action = 'state.scope_add' AND l.target = 'state:' || s.id
          AND (l.details->>'app' = p.id
               OR (l.actor_account_id IS NULL AND l.details ? 'reason'
                   AND l.details->'scopes' @> to_jsonb(p.user_scopes)))
      )
    ON CONFLICT DO NOTHING
    RETURNING state_id, plugin_id
),
dropped AS (
    DELETE FROM core.state_scopes x
    USING converted c
    JOIN core.plugins p ON p.id = c.plugin_id
    WHERE x.state_id = c.state_id AND x.scope = ANY(p.user_scopes)
    RETURNING x.state_id, x.scope
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'state.app_required', 'state:' || c.state_id,
       jsonb_build_object(
           'state', s.name,
           'app', c.plugin_id,
           'scopes_dropped', (SELECT coalesce(jsonb_agg(DISTINCT d.scope), '[]'::jsonb)
                              FROM dropped d WHERE d.state_id = c.state_id),
           'reason', 'upgrade: a state requiring an app''s scopes requires registering for the app, as AA''s Member Audit compliance'
       )
FROM converted c
JOIN core.states s ON s.id = c.state_id;

-- Registering from the state's checklist (not a plain Add Character) is
-- its own login purpose: it registers for the apps the state requires.
ALTER TABLE core.login_attempts DROP CONSTRAINT login_attempts_purpose_check;
ALTER TABLE core.login_attempts ADD CONSTRAINT login_attempts_purpose_check
    CHECK (purpose IN ('login', 'register', 'register_state', 'register_app', 'data_source',
                       'change_main', 'reauth'));

-- Nobody's compliance changes: every character of those states' accounts
-- whose token carries a required app's scopes is registered for it (as
-- 0050 registered Members' characters then), by the system, audited.
WITH registered AS (
    INSERT INTO core.app_characters (plugin_id, character_id, registered_by)
    SELECT a.plugin_id, c.id, NULL
    FROM core.state_apps a
    JOIN core.plugins p ON p.id = a.plugin_id
    JOIN core.accounts acc ON acc.state_id = a.state_id
    JOIN core.characters c ON c.account_id = acc.id
    JOIN core.character_tokens t ON t.character_id = c.id
    WHERE t.scopes @> p.user_scopes
    ON CONFLICT DO NOTHING
    RETURNING plugin_id, character_id
)
INSERT INTO core.audit_log (actor_account_id, action, target, details)
SELECT NULL, 'plugin.characters_registered', 'plugin:' || plugin_id,
       jsonb_build_object(
           'characters', count(*),
           'reason', 'upgrade: states now require registering for the app, so characters meeting its scopes were registered'
       )
FROM registered
GROUP BY plugin_id;
