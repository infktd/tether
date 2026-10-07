-- aa-afat's ESI error handling: tracking stops only when the same error
-- comes back after 3 in a row, each within 75 seconds of the last. The
-- last error, when, and how many in a row; a good read clears them.
ALTER TABLE links
    ADD COLUMN esi_error text,
    ADD COLUMN esi_error_at timestamptz,
    ADD COLUMN esi_errors integer NOT NULL DEFAULT 0;
