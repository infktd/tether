-- The pilot sees their request's history (aa-srp's request details): the
-- decisions, payouts and payments, with their comments, from now on. SRP
-- staff's own comments stay theirs, and so does every comment made before,
-- as the page said then.
ALTER TABLE comments ADD COLUMN shown boolean NOT NULL DEFAULT false;
