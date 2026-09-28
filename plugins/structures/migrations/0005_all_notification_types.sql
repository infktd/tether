-- The rest of aa-structures' notification types: sovereignty and bills,
-- wars, members and projects (each kind its own channel, not sent until
-- one is picked), ownership and reinforcement changes, refuelled
-- structures and jump gates low on liquid ozone.
ALTER TABLE settings
    ADD COLUMN sov_channel text,
    ADD COLUMN war_channel text,
    ADD COLUMN corp_channel text;

ALTER TABLE owner_channels DROP CONSTRAINT owner_channels_category_check;
ALTER TABLE owner_channels ADD CONSTRAINT owner_channels_category_check
    CHECK (category IN ('attack', 'fuel', 'state', 'moon', 'sov', 'war', 'corp'));

-- aa-structures' is_alliance_main: alliance-wide notifications (sovereignty,
-- most wars, bills) are sent only for this owner of its alliance, since
-- every corporation in the alliance gets them. The alliance it's main of:
-- the flag lapses if the corporation moves to another.
ALTER TABLE owner_settings ADD COLUMN alliance_main bigint;

-- When a sync character's corporation last changed: notifications about
-- a corporation from before it are its last corporation's, not sent.
ALTER TABLE owners ADD COLUMN corporation_since timestamptz;

-- Who sent a notification (sovereignty names the alliance holding it), and
-- whether it's about one of the owner's structures (sent once that
-- structure is known) or the corporation (sent at once).
ALTER TABLE notifications
    ADD COLUMN sender_id bigint,
    ADD COLUMN structure_related boolean NOT NULL DEFAULT true;

-- Sovereignty timers (a TCU or IHub reinforced), for Structure Timers.
CREATE TABLE sov_timers (
    system_id bigint NOT NULL,
    structure text NOT NULL,
    at timestamptz NOT NULL,
    corporation_id bigint NOT NULL,
    holder text NOT NULL DEFAULT '',
    PRIMARY KEY (system_id, structure, at)
);
CREATE INDEX sov_timers_at_idx ON sov_timers (at);

-- The fuel expiry last seen, to tell a refuel (aa-structures' refueled
-- notifications) from the clock running down.
ALTER TABLE structures ADD COLUMN refuel_seen timestamptz;

-- aa-structures' jump fuel alert configs: an alert once a jump gate's
-- liquid ozone is below the threshold, until it's topped up above it.
CREATE TABLE jump_fuel_alert_configs (
    id serial PRIMARY KEY,
    threshold integer NOT NULL CHECK (threshold > 0),
    ping text NOT NULL DEFAULT 'none' CHECK (ping IN ('none', 'warning', 'danger')),
    enabled boolean NOT NULL DEFAULT true
);
CREATE TABLE jump_fuel_alerts_sent (
    structure_id bigint NOT NULL,
    config_id integer NOT NULL REFERENCES jump_fuel_alert_configs (id) ON DELETE CASCADE,
    PRIMARY KEY (structure_id, config_id)
);
