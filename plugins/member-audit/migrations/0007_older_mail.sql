-- Mail headers come from ESI 50 a call, newest first, and older ones a
-- few pages a read until the Settings' mails kept per character are
-- stored (as aa-memberaudit pages them with last_mail_id). Whether ESI
-- may hold older mail than the oldest stored, still to be read: false
-- once a read finds no more, or only mail older than the Settings keep.
-- Characters read before this kept only ESI's newest 50 a read, so they
-- start with it true too.
ALTER TABLE characters ADD COLUMN mail_older boolean NOT NULL DEFAULT true;
