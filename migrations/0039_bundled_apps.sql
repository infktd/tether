-- Apps bundled into Tether's image: first-party apps that ship with every
-- deployment. They're as trusted as the binary they ship with, so they
-- carry no signature and pin no key; everything else is still a signed
-- package. Where each installed package (and the one an upgrade replaced)
-- came from.
ALTER TABLE core.plugins
    ADD COLUMN origin text NOT NULL DEFAULT 'signed'
        CHECK (origin IN ('signed', 'bundled')),
    ADD COLUMN previous_origin text
        CHECK (previous_origin IN ('signed', 'bundled'));

-- origin keeps its default: a row that doesn't say is a signed package,
-- and must have its signature (below).
UPDATE core.plugins SET previous_origin = 'signed' WHERE previous_package IS NOT NULL;

-- A signed package has its signature; only a bundled one has none.
ALTER TABLE core.plugins
    ALTER COLUMN signature DROP NOT NULL,
    ADD CONSTRAINT plugins_signed CHECK ((origin = 'signed') = (signature IS NOT NULL)),
    ADD CONSTRAINT plugins_previous_signed CHECK (
        CASE WHEN previous_origin IS NULL THEN previous_signature IS NULL
             ELSE (previous_origin = 'signed') = (previous_signature IS NOT NULL)
        END
    );

-- The previous package's parts stay together (its signature, when it was
-- signed: above).
ALTER TABLE core.plugins DROP CONSTRAINT plugins_previous_whole;
ALTER TABLE core.plugins ADD CONSTRAINT plugins_previous_whole CHECK (
    num_nulls(previous_version, previous_package, previous_origin,
              previous_package_sha256, upgraded_at) IN (0, 5)
);
