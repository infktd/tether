-- Fleet Operations' own schema (the plugin's; the host runs this once).
-- AA's optimer: OpTimer and OpTimerType, with the character kept as a
-- name snapshot.

-- Operation types, made as they're first used (AA's OpTimerType), one per
-- name whatever its case (AA finds them with iexact).
CREATE TABLE op_types (
    id bigserial PRIMARY KEY,
    name text NOT NULL
);
CREATE UNIQUE INDEX op_types_name_idx ON op_types (lower(name));

CREATE TABLE ops (
    id bigserial PRIMARY KEY,
    operation_name text NOT NULL,
    doctrine text NOT NULL,
    -- The form-up system, as typed.
    system text NOT NULL,
    start_time timestamptz NOT NULL,
    -- Free text, as AA's ("2h", "until done").
    duration text NOT NULL,
    fc text NOT NULL,
    description text NOT NULL DEFAULT '',
    type_id bigint REFERENCES op_types (id) ON DELETE SET NULL,
    -- AA's eve_character: the main of whoever last saved it.
    account_id bigint NOT NULL,
    character_id bigint NOT NULL,
    character_name text NOT NULL,
    -- AA's post_time: when it was first posted.
    post_time timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ops_start_time_idx ON ops (start_time);
