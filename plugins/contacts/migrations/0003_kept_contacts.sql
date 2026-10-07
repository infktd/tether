-- A contact gone from EVE's list is kept while it has notes or server
-- links, at standing 0 without labels (aa-contacts): marked here, so the
-- list can say it's no longer in EVE's.
ALTER TABLE contacts ADD COLUMN in_eve boolean NOT NULL DEFAULT true;
