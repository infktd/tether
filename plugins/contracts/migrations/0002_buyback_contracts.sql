-- Contracts leaves Buyback's contracts to Buyback (Jay, 2026-10-07): item
-- exchanges whose title carries a tracking number, kept before, go, with
-- the notices noted for them.
DELETE FROM notices WHERE contract_id IN (
    SELECT contract_id FROM contracts
    WHERE type = 'item_exchange'
      AND title ~ '(^|\s)\S+-(R-)?[0-9]+-[0-9a-f]{6}(\s|$)'
);
DELETE FROM contracts
WHERE type = 'item_exchange'
  AND title ~ '(^|\s)\S+-(R-)?[0-9]+-[0-9a-f]{6}(\s|$)';
