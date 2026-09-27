-- As AA: a loss is claimed by its request alone (requests.killmail_id is
-- unique), so once a fleet and its requests are removed, the loss can be
-- requested again. The separate record of every loss ever claimed goes.
DROP TABLE claimed_kills;
