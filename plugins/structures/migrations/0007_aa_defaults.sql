-- aa-structures' fresh-install defaults (Jay, 2026-10-06: parity across the
-- board). An instance already in use keeps everything it stored: they
-- apply only where Structures hasn't been used yet, judged once, before any
-- of them changes anything.

-- A fresh install: no owner, nothing read, queued or alerted, no channel
-- picked, and every setting as 0001-0006 left it. Plugin migrations run at
-- install, before any job, so a new install always is one; an instance
-- with owners never is. Then, as aa-structures starts:
-- - default pings on (its webhooks' and owners' has_default_pings_enabled),
--   danger and warning both mentioning Member's Discord role (its
--   @everyone and @here: a role mention has no online-only form);
-- - its 22 default notification types for new webhooks (webhook_defaults());
-- - no fuel alert configs (it seeds none, so EVE's own fuel alerts report
--   low fuel; 0004's three reported every low-fuel structure twice).
-- One statement, so each part sees the install as it was.
WITH fresh AS (
    SELECT NOT EXISTS (SELECT 1 FROM owners)
       AND NOT EXISTS (SELECT 1 FROM owner_settings)
       AND NOT EXISTS (SELECT 1 FROM owner_channels)
       AND NOT EXISTS (SELECT 1 FROM structures)
       AND NOT EXISTS (SELECT 1 FROM notifications)
       AND NOT EXISTS (SELECT 1 FROM outbox)
       AND NOT EXISTS (SELECT 1 FROM fuel_alerts_sent)
       AND NOT EXISTS (SELECT 1 FROM jump_fuel_alert_configs)
       AND NOT EXISTS (SELECT 1 FROM tags WHERE is_user_managed)
       AND (SELECT count(*) FROM fuel_alert_configs) = 3
       AND (SELECT count(*) FROM fuel_alert_configs
            WHERE (start_hours, end_hours) IN ((72, 24), (24, 6), (6, 0))
              AND repeat_hours = 0 AND ping = 'none' AND enabled) = 3
       AND EXISTS (SELECT 1 FROM settings WHERE id = 1
            AND num_nonnulls(attack_channel, fuel_channel, state_channel, moon_channel,
                             sov_channel, war_channel, corp_channel) = 0
            AND notification_types IS NULL
            AND NOT default_pings AND danger_ping IS NOT DISTINCT FROM 'Member'
            AND warning_ping IS NULL
            AND NOT timers_corporation_only AND NOT default_tags_filter) AS yes
), seeded AS (
    DELETE FROM fuel_alert_configs WHERE (SELECT yes FROM fresh)
)
UPDATE settings SET
    default_pings = true,
    warning_ping = 'Member',
    notification_types = ARRAY[
        'OrbitalAttacked', 'OrbitalReinforced', 'SkyhookDestroyed', 'SkyhookLostShields',
        'SkyhookOnline', 'SkyhookUnderAttack', 'SovStructureDestroyed', 'SovStructureReinforced',
        'StructureAnchoring', 'StructureDestroyed', 'StructureFuelAlert', 'StructureLostArmor',
        'StructureLostShields', 'StructureLowReagentsAlert', 'StructureNoReagentsAlert',
        'StructureOnline', 'StructureServicesOffline', 'StructureUnderAttack',
        'StructureWentHighPower', 'StructureWentLowPower', 'TowerAlertMsg', 'TowerResourceAlertMsg'
    ]
WHERE id = 1 AND (SELECT yes FROM fresh);
