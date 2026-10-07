-- The applicant told when their application is marked in progress,
-- approved, rejected or deleted (AA's notify): Tether's reference to the
-- account that applied, which the app may notify though applicants hold
-- none of its permissions (null for applications made before).
ALTER TABLE applications ADD COLUMN submitter text;
