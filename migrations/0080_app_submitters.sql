-- Submitters apps may notify (Jay, 2026-10-07): an app may send a notice to
-- an account that submitted one of its forms (an applicant, a requester),
-- whether or not it holds any of the app's permissions, and nobody else.
-- While handling a pilot's own form post the app asks for a reference to
-- them, one per account and app, random, telling nothing about the account,
-- and later notifies by it, for a year after that pilot last posted one of
-- the app's forms. They go with the app and with the account.
CREATE TABLE core.plugin_submitters (
    plugin_id text NOT NULL REFERENCES core.plugins (id) ON DELETE CASCADE,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    reference text NOT NULL DEFAULT replace(gen_random_uuid()::text, '-', ''),
    last_posted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plugin_id, account_id),
    UNIQUE (plugin_id, reference)
);
CREATE INDEX plugin_submitters_account_idx ON core.plugin_submitters (account_id);
