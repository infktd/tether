-- aa-moonmining's labels: a name, a description and a style, made by
-- whoever runs the app (AA's admin; here `manage`), one on a moon at most
-- to sort and filter the Moons list.
CREATE TABLE labels (
    id serial PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 100),
    description text NOT NULL DEFAULT '',
    style text NOT NULL DEFAULT 'default'
);

ALTER TABLE moons ADD COLUMN label_id integer REFERENCES labels ON DELETE SET NULL;
