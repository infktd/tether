-- Mail headers are stored a page at a time as they're read. A read cut
-- short part way down new mail leaves the mail before this id, down to
-- the newest stored below it, still to read: the next read goes on from
-- here first.
ALTER TABLE characters ADD COLUMN mail_gap bigint;
