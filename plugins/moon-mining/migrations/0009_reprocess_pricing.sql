-- aa-moonmining's MOONMINING_USE_REPROCESS_PRICING (off) and
-- MOONMINING_REPROCESSING_YIELD (0.85): ores priced by what they refine
-- into instead of their own average price.
ALTER TABLE settings
    ADD COLUMN reprocess_pricing boolean NOT NULL DEFAULT false,
    ADD COLUMN reprocessing_yield double precision NOT NULL DEFAULT 0.85
        CHECK (reprocessing_yield > 0 AND reprocessing_yield <= 1);

-- What one portion of each ore Moon Mining values refines into, from the
-- static data Tether bundles (the catalogue's sde-materials), read daily
-- with the prices.
CREATE TABLE ore_materials (
    type_id bigint NOT NULL,
    material_id bigint NOT NULL,
    quantity bigint NOT NULL CHECK (quantity > 0),
    portion_size integer NOT NULL CHECK (portion_size > 0),
    PRIMARY KEY (type_id, material_id)
);

-- The unit price every value uses (aa-moonmining's current_price): the
-- ore's average price, else its adjusted price, or with reprocess pricing
-- on its refined materials' worth. Worked out after each price read and
-- each change of the settings.
ALTER TABLE prices ADD COLUMN unit_price double precision;
UPDATE prices SET unit_price = coalesce(average_price, adjusted_price);
