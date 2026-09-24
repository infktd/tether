-- Postgres is the queue (no broker). Workers claim with
-- FOR UPDATE SKIP LOCKED and hold a lease in locked_until; a job whose
-- lease expires (worker crashed) can be claimed again.
CREATE TABLE core.jobs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    state text NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued', 'running', 'succeeded', 'dead')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    run_at timestamptz NOT NULL DEFAULT now(),
    locked_until timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE INDEX jobs_ready_idx ON core.jobs (run_at, id) WHERE state = 'queued';
CREATE INDEX jobs_leased_idx ON core.jobs (locked_until) WHERE state = 'running';
