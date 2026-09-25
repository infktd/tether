-- In-app notifications, Alliance Auth style (F23): danger, warning, info
-- or success; at most 50 per account (Tether trims the oldest).
CREATE TABLE core.notifications (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    account_id bigint NOT NULL REFERENCES core.accounts (id) ON DELETE CASCADE,
    level text NOT NULL CHECK (level IN ('danger', 'warning', 'info', 'success')),
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 254),
    message text NOT NULL CHECK (length(message) BETWEEN 1 AND 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    read_at timestamptz
);
CREATE INDEX notifications_account_idx ON core.notifications (account_id, id DESC);

-- Tells the app (LISTEN tether_notifications) whose unread count moved, so
-- open pages update live, whichever process made the change.
CREATE FUNCTION core.notifications_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_notify(
        'tether_notifications',
        COALESCE(NEW.account_id, OLD.account_id)::text
    );
    RETURN NULL;
END;
$$;
CREATE TRIGGER notifications_added_or_removed
    AFTER INSERT OR DELETE ON core.notifications
    FOR EACH ROW EXECUTE FUNCTION core.notifications_changed();
-- Only a real change of read state: re-opening a read one is silent.
CREATE TRIGGER notifications_read
    AFTER UPDATE OF read_at ON core.notifications
    FOR EACH ROW WHEN (OLD.read_at IS DISTINCT FROM NEW.read_at)
    EXECUTE FUNCTION core.notifications_changed();

-- When a Corporation Stats source started failing, and whether its owner
-- was told: once, after a day (not for a passing ESI outage).
ALTER TABLE core.corp_sources
    ADD COLUMN failing_since timestamptz,
    ADD COLUMN failure_notified boolean NOT NULL DEFAULT false;
