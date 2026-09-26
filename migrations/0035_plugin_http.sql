-- Plugin HTTP: the hosts and secrets an admin approved for each plugin,
-- and a log of every request. What runs is what was approved here: a host
-- or secret a later package declares is refused until it is approved too.
CREATE TABLE core.plugin_http_hosts (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    host text NOT NULL,
    approved_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    approved_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, host)
);

-- Where each approved secret goes. Values are sealed in core.secrets as
-- `plugin-secret:<id>:<name>`, entered by an admin on the plugin's page.
CREATE TABLE core.plugin_http_secrets (
    plugin_id text NOT NULL,
    name text NOT NULL,
    host text NOT NULL,
    header text NOT NULL,
    prefix text,
    approved_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    approved_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, name),
    FOREIGN KEY (plugin_id, host) REFERENCES core.plugin_http_hosts (plugin_id, host)
        ON DELETE CASCADE
);

-- Every request a plugin made or tried: method, host and path (no query),
-- the outcome, and how big and slow it was. Pruned by age and count.
CREATE TABLE core.plugin_http_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plugin_id text NOT NULL,
    method text NOT NULL,
    host text NOT NULL,
    path text NOT NULL,
    -- NULL when no answer came back
    status integer,
    -- ok, or why it was refused or failed
    outcome text NOT NULL,
    -- the secret added, by name
    secret text,
    bytes bigint NOT NULL DEFAULT 0,
    duration_ms integer NOT NULL DEFAULT 0,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX plugin_http_log_recent_idx ON core.plugin_http_log (plugin_id, id DESC);
CREATE INDEX plugin_http_log_at_idx ON core.plugin_http_log (at);

-- Plugins installed before this migration were approved with the hosts
-- and secrets their install review showed, recorded in the audit log.
WITH installs AS (
    SELECT DISTINCT ON (a.target) substring(a.target FROM 8) AS plugin_id,
           a.details -> 'capabilities' AS capabilities, a.actor_account_id, a.at
    FROM core.audit_log a
    WHERE a.action = 'plugin.installed' AND a.target LIKE 'plugin:%'
    ORDER BY a.target, a.id DESC
)
INSERT INTO core.plugin_http_hosts (plugin_id, host, approved_by, approved_at)
SELECT i.plugin_id, h.host, acc.id, i.at
FROM installs i
JOIN core.plugins p ON p.id = i.plugin_id
CROSS JOIN LATERAL jsonb_array_elements_text(
    CASE WHEN jsonb_typeof(i.capabilities -> 'http') = 'array'
         THEN i.capabilities -> 'http' ELSE '[]'::jsonb END
) AS h(host)
LEFT JOIN core.accounts acc ON acc.id = i.actor_account_id
ON CONFLICT DO NOTHING;

WITH installs AS (
    SELECT DISTINCT ON (a.target) substring(a.target FROM 8) AS plugin_id,
           a.details -> 'capabilities' AS capabilities, a.actor_account_id, a.at
    FROM core.audit_log a
    WHERE a.action = 'plugin.installed' AND a.target LIKE 'plugin:%'
    ORDER BY a.target, a.id DESC
)
INSERT INTO core.plugin_http_secrets (plugin_id, name, host, header, prefix, approved_by, approved_at)
SELECT i.plugin_id, s.key, s.value ->> 'host', s.value ->> 'header', s.value ->> 'prefix',
       acc.id, i.at
FROM installs i
JOIN core.plugins p ON p.id = i.plugin_id
CROSS JOIN LATERAL jsonb_each(
    CASE WHEN jsonb_typeof(i.capabilities -> 'secrets') = 'object'
         THEN i.capabilities -> 'secrets' ELSE '{}'::jsonb END
) AS s(key, value)
JOIN core.plugin_http_hosts h ON h.plugin_id = i.plugin_id AND h.host = s.value ->> 'host'
LEFT JOIN core.accounts acc ON acc.id = i.actor_account_id
WHERE s.value ->> 'header' IS NOT NULL
ON CONFLICT DO NOTHING;
