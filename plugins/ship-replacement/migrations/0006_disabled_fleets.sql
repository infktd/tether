-- AA core's disabled SRP fleet (aa-srp's Closed): no requests for now, until
-- SRP staff enable it, without completing it.
ALTER TABLE fleets ADD COLUMN disabled boolean NOT NULL DEFAULT false;
