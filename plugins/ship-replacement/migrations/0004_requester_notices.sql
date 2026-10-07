-- The pilot told when their request is approved or rejected (AA core's
-- srp notify): Tether's reference to the account that requested it, which
-- the app may notify whatever it holds (null for requests made before).
ALTER TABLE requests ADD COLUMN submitter text;
