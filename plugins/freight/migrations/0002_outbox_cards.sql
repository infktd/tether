-- The Discord card a notice posts as (card::embed reads it); none sends
-- the message as plain text.
ALTER TABLE outbox ADD COLUMN card jsonb;
