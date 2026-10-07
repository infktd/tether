-- A mail's body and a contract's items are read once and kept. Only ESI
-- saying they're gone (404) settles them empty; a passing failure (a 5xx
-- around downtime, a 420) leaves them to be asked again on a later read,
-- after those not tried yet. When each was last tried and failed:
ALTER TABLE mails ADD COLUMN body_tried_at timestamptz;
ALTER TABLE contracts ADD COLUMN items_tried_at timestamptz;
