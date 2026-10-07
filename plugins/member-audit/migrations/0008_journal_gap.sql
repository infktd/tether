-- The wallet journal is stored a page at a time as it's read. Whether
-- what's stored may miss entries below its newest (a read cut short or
-- read in part, a first read, a longer keep): reads then go down every
-- page ESI has, and the sheet says the journal is partial until one gets
-- through. Characters read before this kept only ESI's first page a read,
-- so they start with it too.
ALTER TABLE characters ADD COLUMN journal_gap boolean NOT NULL DEFAULT true;
