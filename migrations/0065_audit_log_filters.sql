-- The audit log filtered by who, action, app and date (Administration's
-- Audit log and its CSV), newest first by (at, id): each filter reads in
-- that order from an index of its own, within the dates asked for. The
-- dates alone use audit_log_at_idx (at DESC, id DESC).
CREATE INDEX audit_log_actor_idx ON core.audit_log (actor_account_id, at DESC, id DESC);
CREATE INDEX audit_log_action_idx ON core.audit_log (action, at DESC, id DESC);

-- The app an entry is about: its target (`plugin:<id>`, or one of the
-- app's schedules, `schedule:plugin:<id>:<name>`), else its details'
-- `app` (a state requiring an app).
CREATE FUNCTION core.audit_log_app(target text, details jsonb) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE
        WHEN target LIKE 'plugin:%' THEN substring(target FROM 8)
        WHEN target LIKE 'schedule:plugin:%' THEN split_part(substring(target FROM 17), ':', 1)
        ELSE details ->> 'app'
    END
    $$;

CREATE INDEX audit_log_app_idx ON core.audit_log (core.audit_log_app(target, details), at DESC, id DESC)
    WHERE core.audit_log_app(target, details) IS NOT NULL;
