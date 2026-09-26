-- HR Applications' own schema (the plugin's; the host runs this once).
-- AA's hrapplications: a form per corporation with its questions,
-- applications with their answers, and reviewers' comments.

CREATE TABLE forms (
    id bigserial PRIMARY KEY,
    corporation_id bigint NOT NULL UNIQUE,
    corporation_name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE questions (
    id bigserial PRIMARY KEY,
    form_id bigint NOT NULL REFERENCES forms ON DELETE CASCADE,
    position integer NOT NULL,
    title text NOT NULL,
    help_text text NOT NULL DEFAULT '',
    -- A JSON array of text; empty means a written answer.
    choices jsonb NOT NULL DEFAULT '[]',
    -- With choices: tick any of them, rather than pick one.
    multi_select boolean NOT NULL DEFAULT false
);
CREATE INDEX questions_form_idx ON questions (form_id, position);

CREATE TABLE applications (
    id bigserial PRIMARY KEY,
    form_id bigint NOT NULL REFERENCES forms ON DELETE CASCADE,
    account_id bigint NOT NULL,
    main_character_id bigint NOT NULL,
    main_name text NOT NULL,
    main_corporation_id bigint NOT NULL,
    -- The account's characters when they applied:
    -- [{"id", "name", "corporation_id", "alliance_id"}].
    characters jsonb NOT NULL,
    -- Null while pending; then approved (true) or rejected (false).
    approved boolean,
    reviewer_account_id bigint,
    reviewer_character_id bigint,
    reviewer_name text,
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz,
    -- One application per corporation and account, as AA.
    UNIQUE (form_id, account_id)
);
CREATE INDEX applications_account_idx ON applications (account_id);

CREATE TABLE responses (
    application_id bigint NOT NULL REFERENCES applications ON DELETE CASCADE,
    position integer NOT NULL,
    -- The question as it was asked, so later edits don't change answers.
    question text NOT NULL,
    answer text NOT NULL,
    PRIMARY KEY (application_id, position)
);

CREATE TABLE comments (
    id bigserial PRIMARY KEY,
    application_id bigint NOT NULL REFERENCES applications ON DELETE CASCADE,
    author_account_id bigint NOT NULL,
    author_character_id bigint NOT NULL,
    author_name text NOT NULL,
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX comments_application_idx ON comments (application_id, created_at);
