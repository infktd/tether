-- As AA: a loss is claimed by its request alone (requests.killmail_id is
-- unique), so once a fleet and its requests are removed, the loss can be
-- requested again.
--
-- Losses of fleets removed before this version were promised to stay
-- claimed (and may have been paid): they stay so, as legacy claims. Those
-- with a request still in place need no record of their own.
DELETE FROM claimed_kills WHERE killmail_id IN (SELECT killmail_id FROM requests);
ALTER TABLE claimed_kills RENAME TO legacy_claims;
