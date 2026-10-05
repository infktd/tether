-- Notices apps send (the notify interface; approved by Jay, 2026-10-04):
-- which app sent one, shown beside it so no app passes for Tether, and so
-- each app keeps only a few of an account's notices. They go with the app.
ALTER TABLE core.notifications
    ADD COLUMN plugin_id text REFERENCES core.plugins (id) ON DELETE CASCADE;
CREATE INDEX notifications_plugin_idx ON core.notifications (account_id, plugin_id, id DESC)
    WHERE plugin_id IS NOT NULL;
