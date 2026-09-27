-- aa-afat's use_doctrines_from_fittings_module: Create FAT Link offers the
-- doctrines Fittings shares that the FC may see, instead of free text.
ALTER TABLE settings ADD COLUMN use_doctrines_from_fittings boolean NOT NULL DEFAULT false;
