-- aa-contacts' tracked alliances and corporations: one per owner's
-- corporation and alliance, with when their contacts were last read.
CREATE TABLE tracked (
    kind text NOT NULL CHECK (kind IN ('alliance', 'corporation')),
    entity_id bigint NOT NULL,
    updated_at timestamptz,
    last_error text,
    PRIMARY KEY (kind, entity_id)
);

-- Their contacts and standings; notes are Tether's, kept across updates.
CREATE TABLE contacts (
    kind text NOT NULL,
    entity_id bigint NOT NULL,
    contact_id bigint NOT NULL,
    contact_type text NOT NULL,
    standing double precision NOT NULL,
    label_ids text NOT NULL DEFAULT '',
    notes text NOT NULL DEFAULT '' CHECK (char_length(notes) <= 2000),
    PRIMARY KEY (kind, entity_id, contact_id),
    FOREIGN KEY (kind, entity_id) REFERENCES tracked (kind, entity_id) ON DELETE CASCADE
);

CREATE TABLE labels (
    kind text NOT NULL,
    entity_id bigint NOT NULL,
    label_id bigint NOT NULL,
    name text NOT NULL,
    PRIMARY KEY (kind, entity_id, label_id),
    FOREIGN KEY (kind, entity_id) REFERENCES tracked (kind, entity_id) ON DELETE CASCADE
);

-- aa-contacts' server links on a contact: a name, an address (any
-- scheme: Discord invites, TeamSpeak), an optional password and a colour.
CREATE TABLE server_links (
    id serial PRIMARY KEY,
    kind text NOT NULL,
    entity_id bigint NOT NULL,
    contact_id bigint NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 100),
    url text NOT NULL CHECK (char_length(url) BETWEEN 1 AND 500),
    password text NOT NULL DEFAULT '' CHECK (char_length(password) <= 255),
    color text NOT NULL DEFAULT 'secondary',
    FOREIGN KEY (kind, entity_id, contact_id)
        REFERENCES contacts (kind, entity_id, contact_id) ON DELETE CASCADE
);

-- Names ESI gave: contacts, alliances, corporations.
CREATE TABLE names (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
