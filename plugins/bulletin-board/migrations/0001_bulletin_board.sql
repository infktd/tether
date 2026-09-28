-- aa-bulletin-board's bulletins: a title, the text, who wrote it and when,
-- and the groups it's limited to (none: everyone with access).
CREATE TABLE bulletins (
    id serial PRIMARY KEY,
    title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 255),
    content text NOT NULL CHECK (char_length(content) BETWEEN 1 AND 10000),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz,
    author_id bigint NOT NULL,
    author_name text NOT NULL
);
CREATE INDEX bulletins_created_idx ON bulletins (created_at DESC);

CREATE TABLE bulletin_groups (
    bulletin_id integer NOT NULL REFERENCES bulletins (id) ON DELETE CASCADE,
    group_id bigint NOT NULL,
    PRIMARY KEY (bulletin_id, group_id)
);
