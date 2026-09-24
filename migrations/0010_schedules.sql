-- Recurring jobs. The scheduler enqueues a job when next_run_at passes,
-- unless the previous run is still queued or running.
CREATE TABLE core.schedules (
    name text PRIMARY KEY,
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    every_secs integer NOT NULL CHECK (every_secs > 0),
    enabled boolean NOT NULL DEFAULT true,
    next_run_at timestamptz NOT NULL DEFAULT now(),
    last_enqueued_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Which schedule enqueued a job, if any.
ALTER TABLE core.jobs ADD COLUMN schedule text;
CREATE INDEX jobs_active_schedule_idx ON core.jobs (schedule)
    WHERE schedule IS NOT NULL AND state IN ('queued', 'running');
