-- aa-memberaudit's data exports (contracts, contract items, wallet journal)
-- carry these too; rows already read keep them empty until read again.
ALTER TABLE journal
    ADD COLUMN context_id bigint,
    ADD COLUMN context_id_type text,
    ADD COLUMN tax double precision,
    ADD COLUMN tax_receiver_id bigint,
    ADD COLUMN reason text NOT NULL DEFAULT '';
ALTER TABLE contracts
    ADD COLUMN accepted timestamptz,
    ADD COLUMN issuer_corporation_id bigint,
    ADD COLUMN days_to_complete integer,
    ADD COLUMN buyout double precision;
ALTER TABLE contract_items
    ADD COLUMN is_singleton boolean NOT NULL DEFAULT false,
    -- ESI's: -1 a blueprint original, -2 a copy.
    ADD COLUMN raw_quantity bigint;

-- When each topic's export was last asked for (AA: at most once an hour).
CREATE TABLE export_runs (
    topic text PRIMARY KEY,
    asked_at timestamptz NOT NULL
);
