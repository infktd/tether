-- Apps' Secure Groups filters (aa-securegroups takes skills, assets and FATs
-- from apps) and timers apps share (aa-structures feeding the timerboard).

ALTER TABLE core.smart_filters DROP CONSTRAINT smart_filters_kind_check;
ALTER TABLE core.smart_filters ADD CONSTRAINT smart_filters_kind_check CHECK (kind IN
    ('state', 'main_affiliation', 'any_affiliation', 'character_age', 'groups', 'compliant', 'app'));

-- A plugin's value per character for one filter setting. Only settings
-- smart groups use are accepted; values over two days old count as
-- unknown. The host combines characters into accounts: plugins never learn
-- which characters share one.
CREATE TABLE core.plugin_filter_values (
    plugin_id text NOT NULL,
    name text NOT NULL,
    config text NOT NULL,
    character_id bigint NOT NULL,
    value bigint NOT NULL,
    reported_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, name, config, character_id)
);

-- When each setting was last reported, empty reports included: a setting
-- is known while this is under two days old.
CREATE TABLE core.plugin_filter_reports (
    plugin_id text NOT NULL,
    name text NOT NULL,
    config text NOT NULL,
    reported_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, name, config)
);

-- Timers an app publishes for others to show.
CREATE TABLE core.shared_timers (
    plugin_id text NOT NULL,
    key text NOT NULL CHECK (length(key) BETWEEN 1 AND 100),
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    at timestamptz NOT NULL,
    system text NOT NULL CHECK (length(system) <= 100),
    details text NOT NULL CHECK (length(details) <= 1000),
    objective text NOT NULL CHECK (objective IN ('friendly', 'hostile', 'neutral')),
    corporation_id bigint,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, key)
);
CREATE INDEX shared_timers_at_idx ON core.shared_timers (at);
