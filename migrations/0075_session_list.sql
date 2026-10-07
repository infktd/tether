-- The account's Sessions page (crates/web-core/src/sessions.rs): each
-- session gets a plain number to name it by in forms, since its key is the
-- hash of the cookie and never leaves the server. `device` is a coarse
-- label read once from the browser's User-Agent at sign-in, from a fixed
-- list ("Firefox on Windows"); the User-Agent itself is never stored, and
-- nor is an IP address. Sessions from before this have none.
ALTER TABLE core.sessions ADD COLUMN id bigint GENERATED ALWAYS AS IDENTITY;
ALTER TABLE core.sessions ADD CONSTRAINT sessions_id_key UNIQUE (id);
ALTER TABLE core.sessions ADD COLUMN device text CHECK (length(device) BETWEEN 1 AND 40);
