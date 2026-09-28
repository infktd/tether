-- What the Discord card around a message shows (card::Card, as JSON);
-- none sends the message as plain text.
ALTER TABLE outbox ADD COLUMN card jsonb;
