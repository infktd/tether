-- Every page of a character's assets is read, as aa-memberaudit reads
-- them: a long list over several runs, MAX_PAGES at a time. Pages read so
-- far wait here; the stored assets (and the Secure Groups filter's
-- assets_at) are replaced only once the last page is in.
CREATE TABLE assets_reading (
    character_id bigint NOT NULL REFERENCES characters ON DELETE CASCADE,
    item_id bigint NOT NULL,
    type_id bigint NOT NULL,
    quantity bigint NOT NULL,
    location_id bigint NOT NULL,
    location_flag text NOT NULL,
    location_type text,
    PRIMARY KEY (character_id, item_id)
);

-- The next page to read and how many ESI said there were when the read
-- began; NULL between reads.
ALTER TABLE characters ADD COLUMN assets_page integer;
ALTER TABLE characters ADD COLUMN assets_pages integer;
