-- aa-structures' rules (Jay, 2026-09-27: "take AA's permissions and their
-- settings").

-- Notification types (aa-structures' per-webhook filters): which types are
-- sent, by default and per owner. NULL: every type (the default), or for
-- an owner, the default's.
ALTER TABLE settings ADD COLUMN notification_types text[];
ALTER TABLE owner_settings ADD COLUMN notification_types text[];

-- Pings by severity (aa-structures' default pings: danger notifications
-- ping @everyone, warnings @here). The bot mentions the Discord role of a
-- state instead: these name the states, NULL for no mention. What was
-- "mention Members on attacks" is now the default pings switch; owners'
-- 'default' / 'on' / 'off' keep working as their own switch.
ALTER TABLE settings RENAME COLUMN mention_members TO default_pings;
ALTER TABLE settings
    ADD COLUMN danger_ping text DEFAULT 'Member' CHECK (length(danger_ping) BETWEEN 1 AND 64),
    ADD COLUMN warning_ping text CHECK (length(warning_ping) BETWEEN 1 AND 64);
-- The state whose role a message mentions (NULL: none). The old boolean
-- stays for messages queued before: true meant Member.
ALTER TABLE outbox ADD COLUMN mention_state text;

-- aa-structures' fuel alert configs, any number: an alert when a
-- structure's fuel runs out in at most start_hours and more than
-- end_hours, again every repeat_hours (0: once) while it stays there, with
-- a ping level. Tether's thresholds become one config each, down to the
-- next.
CREATE TABLE fuel_alert_configs (
    id serial PRIMARY KEY,
    start_hours integer NOT NULL CHECK (start_hours BETWEEN 1 AND 8760),
    end_hours integer NOT NULL CHECK (end_hours >= 0 AND end_hours < start_hours),
    repeat_hours integer NOT NULL DEFAULT 0 CHECK (repeat_hours BETWEEN 0 AND 8760),
    ping text NOT NULL DEFAULT 'none' CHECK (ping IN ('none', 'warning', 'danger')),
    enabled boolean NOT NULL DEFAULT true
);
INSERT INTO fuel_alert_configs (start_hours, end_hours)
SELECT h, coalesce(lead(h) OVER (ORDER BY h DESC), 0)
FROM (SELECT DISTINCT x::integer AS h
      FROM settings, unnest(string_to_array(fuel_thresholds, ',')) AS x
      WHERE id = 1) t
WHERE h BETWEEN 1 AND 8760;
-- What was sent, per structure and config.
CREATE TABLE fuel_alerts_sent (
    structure_id bigint NOT NULL,
    config_id integer NOT NULL REFERENCES fuel_alert_configs (id) ON DELETE CASCADE,
    sent_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (structure_id, config_id)
);
DROP TABLE fuel_alerts;
ALTER TABLE settings DROP COLUMN fuel_thresholds;

-- Up to 10 sync characters per owner, rotated (aa-structures'): the
-- first ten added are used.
ALTER TABLE owners ADD COLUMN added_at timestamptz NOT NULL DEFAULT now();
