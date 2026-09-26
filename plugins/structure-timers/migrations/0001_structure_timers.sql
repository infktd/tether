-- Structure Timers' own schema (the plugin's; the host runs this once).
-- AA's timerboard Timer, with the creator kept as a name snapshot.

CREATE TABLE timers (
    id bigserial PRIMARY KEY,
    details text NOT NULL,
    system text NOT NULL,
    planet_moon text NOT NULL DEFAULT '',
    structure text NOT NULL,
    timer_type text NOT NULL,
    objective text NOT NULL CHECK (objective IN ('Friendly', 'Hostile', 'Neutral')),
    eve_time timestamptz NOT NULL,
    important boolean NOT NULL DEFAULT false,
    -- Seen and edited only by pilots whose main is in corporation_id.
    corp_timer boolean NOT NULL DEFAULT false,
    -- The creator's main's corporation when the timer was made.
    corporation_id bigint NOT NULL,
    creator_account_id bigint NOT NULL,
    creator_character_id bigint NOT NULL,
    creator_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- The last editor (a snapshot), if anyone has edited it.
    updated_by_character_id bigint,
    updated_by_name text
);
CREATE INDEX timers_eve_time_idx ON timers (eve_time);
CREATE INDEX timers_corporation_idx ON timers (corporation_id) WHERE corp_timer;
