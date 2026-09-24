-- Plugin jobs share the core queue. A job queued by or for a plugin names
-- it, and may carry the plugin's key: queuing under a key again replaces
-- the queued job, so the key is unique among queued jobs.
-- Uninstalling a plugin takes its jobs with it, including any a call
-- still in flight queues afterwards (that insert fails instead).
ALTER TABLE core.jobs ADD COLUMN plugin_id text REFERENCES core.plugins (id) ON DELETE CASCADE;
ALTER TABLE core.jobs ADD COLUMN job_key text;
-- When a job was meant to run. run_at moves on retries; this doesn't, so a
-- job that runs late (downtime, retries) can be told when it was due.
ALTER TABLE core.jobs ADD COLUMN scheduled_at timestamptz;

CREATE UNIQUE INDEX jobs_plugin_key_idx ON core.jobs (plugin_id, job_key)
    WHERE state = 'queued' AND job_key IS NOT NULL;
CREATE INDEX jobs_plugin_active_idx ON core.jobs (plugin_id)
    WHERE plugin_id IS NOT NULL AND state IN ('queued', 'running');
-- For the cap on jobs a plugin creates per day, and pruning its history.
CREATE INDEX jobs_plugin_created_idx ON core.jobs (plugin_id, created_at)
    WHERE plugin_id IS NOT NULL;

-- What plugins wrote with log.write, from pages and jobs; the newest few
-- are shown on the plugin's admin page and older ones pruned.
CREATE TABLE core.plugin_logs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    at timestamptz NOT NULL DEFAULT now(),
    level text NOT NULL CHECK (level IN ('debug', 'info', 'warn', 'error')),
    -- page:<path> or job:<name>
    source text NOT NULL,
    message text NOT NULL
);
CREATE INDEX plugin_logs_recent_idx ON core.plugin_logs (plugin_id, id DESC);
