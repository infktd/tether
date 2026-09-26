-- Plugin upgrades (F18): the package an upgrade replaced, kept until the
-- next upgrade or a rollback, so going back is one step. Rolling back
-- clears it.
ALTER TABLE core.plugins
    ADD COLUMN previous_version text,
    ADD COLUMN previous_package bytea,
    ADD COLUMN previous_signature text,
    ADD COLUMN previous_package_sha256 bytea,
    ADD COLUMN upgraded_at timestamptz,
    ADD COLUMN upgraded_by bigint REFERENCES core.accounts (id) ON DELETE SET NULL,
    ADD CONSTRAINT plugins_previous_whole CHECK (
        num_nulls(previous_version, previous_package, previous_signature,
                  previous_package_sha256, upgraded_at) IN (0, 5)
    );

ALTER TABLE core.plugins ALTER COLUMN previous_package SET STORAGE EXTERNAL;
