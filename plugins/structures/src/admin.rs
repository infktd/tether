//! aa-structures' admin notices (STRUCTURES_ADMIN_NOTIFICATIONS_ENABLED,
//! its `notify_admins`), in Tether's notifications, to holders of `manage`
//! (superusers always among them, as aa-structures' superusers):
//!
//! - An owner added: "Structure owner added", or "Character added to" for
//!   another data source of a corporation that is already an owner.
//! - Each sync, whether each owner corporation's reads are fresh
//!   (aa-structures' `Owner.update_is_up`): structures within 2 hours,
//!   notifications within 40 minutes, assets (once read: they need the
//!   Director role) within 2 hours. A change is told: down, restored, or
//!   up the first time (enabled). A first look that finds it down is only
//!   stored. An owner not "Included in service status" is judged silently.
//!
//! Best effort: a notice that can't go is logged, and never fails the sync.

use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::log;
use tether_plugin_sdk::notify::{self, Level};
use tether_plugin_sdk::storage::{self, Value as Db};

use crate::notification::clip;
use crate::{int, retry, settings, text};

/// aa-structures' STRUCTURES_STRUCTURE_SYNC_GRACE_MINUTES (structures and
/// assets) and STRUCTURES_NOTIFICATION_SYNC_GRACE_MINUTES.
const STRUCTURE_SYNC_GRACE: &str = "120 minutes";
const NOTIFICATION_SYNC_GRACE: &str = "40 minutes";
/// Notices sent a run at most (the host allows 10 notify calls a run): the
/// rest go next sync.
const NOTICES_PER_RUN: usize = 8;

/// A change in whether an owner's services are up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    Down,
    Restored,
    Enabled,
}

/// aa-structures' transitions: up to down, down to up, and not known yet to
/// up. Not known yet to down is stored without a word.
pub(crate) fn change(was: Option<bool>, now: bool) -> Option<Change> {
    match (was, now) {
        (Some(true), false) => Some(Change::Down),
        (Some(false), true) => Some(Change::Restored),
        (None, true) => Some(Change::Enabled),
        _ => None,
    }
}

/// Whether each of an owner's reads is fresh. Assets: none when they were
/// never read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Health {
    pub structures: bool,
    pub notifications: bool,
    pub assets: Option<bool>,
}

impl Health {
    pub(crate) fn up(self) -> bool {
        self.structures && self.notifications && self.assets.unwrap_or(true)
    }
}

/// A change's notice: level, title and message, on one line (the host makes
/// line breaks spaces).
pub(crate) fn notice(
    change: Change,
    corporation: &str,
    health: Health,
    problem: Option<&str>,
) -> (Level, String, String) {
    let word = |up: bool| if up { "up" } else { "down" };
    let (level, title, message) = match change {
        Change::Down => {
            let assets = health
                .assets
                .map_or("not read (they need the Director role)", word);
            let mut message = format!(
                "Structure services for {corporation} are down. Admin action is likely \
                 required to restore services. Structures: {}; notifications: {}; assets: \
                 {assets}.",
                word(health.structures),
                word(health.notifications),
            );
            if let Some(problem) = problem.map(str::trim).filter(|p| !p.is_empty()) {
                message.push_str(" Last problem: ");
                message.push_str(problem);
            }
            (
                Level::Danger,
                format!("Services are down for {corporation}"),
                message,
            )
        }
        Change::Restored => (
            Level::Success,
            format!("Services restored for {corporation}"),
            format!("Structure services for {corporation} have been restored."),
        ),
        Change::Enabled => (
            Level::Success,
            format!("Services enabled for {corporation}"),
            format!("Structure services for {corporation} have been enabled."),
        ),
    };
    (
        level,
        clip(&title, notify::MAX_TITLE),
        clip(&message, notify::MAX_MESSAGE),
    )
}

/// The notice for owners added: a new corporation, or more data sources for
/// one already an owner. `names`, the new characters (`added` of them);
/// `total`, all of the corporation's.
pub(crate) fn added(
    corporation: &str,
    names: &str,
    added: i64,
    total: i64,
    new_owner: bool,
) -> (String, String) {
    let (title, message) = if new_owner {
        let sources = if added == 1 {
            "its data source"
        } else {
            "its data sources"
        };
        (
            format!("Structure owner added: {corporation}"),
            format!("{corporation} was added as a new structure owner, with {names} as {sources}."),
        )
    } else {
        let verb = if added == 1 { "was" } else { "were" };
        let sources = if total == 1 {
            "1 data source".to_owned()
        } else {
            format!("{total} data sources")
        };
        (
            format!("Character added to: {corporation}"),
            format!(
                "{names} {verb} added as a data source for {corporation}. It now has {sources}."
            ),
        )
    };
    (
        clip(&title, notify::MAX_TITLE),
        clip(&message, notify::MAX_MESSAGE),
    )
}

/// Sends one notice to the admins. Whether it's done with: sent, or refused
/// for good (logged); not when the host couldn't take it now.
fn send(title: &str, message: &str, level: Level) -> bool {
    match notify::holders("manage", title, message, level, None) {
        Ok(_) => true,
        Err(notify::Error::Invalid(why)) => {
            log::warn(format!("an admin notice wasn't sent: {why}"));
            true
        }
        Err(notify::Error::Unavailable) => {
            log::warn("an admin notice couldn't be sent just now: tried again next sync");
            false
        }
    }
}

/// Owners added and services changed since the last sync, as notices when
/// the setting is on.
pub(crate) fn notices() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("admin notices: reading settings", e))?;
    // While the host lists no data sources (ridden out for an hour), owners
    // aren't read: they aren't judged either.
    let missing = storage::query(
        "SELECT sources_missing_since IS NOT NULL FROM settings WHERE id = 1",
        &[],
    )
    .map_err(|e| retry("admin notices: reading settings", e))?;
    if missing
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Db::as_bool)
        .unwrap_or(false)
    {
        return Ok(());
    }
    let mut sent = 0;

    let added_rows = storage::query(
        "SELECT o.corporation_id, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
             string_agg(o.character_name, ', ' ORDER BY o.added_at, o.character_id) \
                 FILTER (WHERE NOT o.announced), \
             count(*) FILTER (WHERE NOT o.announced), count(*), bool_and(NOT o.announced) \
         FROM owners o LEFT JOIN names n ON n.id = o.corporation_id \
         GROUP BY o.corporation_id, n.name HAVING bool_or(NOT o.announced) \
         ORDER BY 1",
        &[],
    )
    .map_err(|e| retry("admin notices: reading owners", e))?;
    for row in &added_rows.rows {
        let corporation = int(row, 0);
        if settings.admin_notifications {
            if sent >= NOTICES_PER_RUN {
                continue;
            }
            let new_owner = row.get(5).and_then(Db::as_bool).unwrap_or(false);
            let (title, message) = added(
                &text(row, 1),
                &text(row, 2),
                int(row, 3),
                int(row, 4),
                new_owner,
            );
            sent += 1;
            if !send(&title, &message, Level::Info) {
                continue;
            }
        }
        storage::execute(
            "UPDATE owners SET announced = true WHERE corporation_id = $1",
            &[corporation.into()],
        )
        .map_err(|e| retry("admin notices: marking owners", e))?;
    }

    let rows = storage::query(
        &format!(
            "SELECT o.corporation_id, coalesce(n.name, 'Corporation ' || o.corporation_id::text), \
                 coalesce(max(o.structures_at) > now() - interval '{STRUCTURE_SYNC_GRACE}', false), \
                 coalesce(max(o.notifications_at) > now() - interval '{NOTIFICATION_SYNC_GRACE}', false), \
                 max(o.assets_at) > now() - interval '{STRUCTURE_SYNC_GRACE}', \
                 s.up, coalesce(w.in_service_status, true), \
                 (array_agg(o.last_error ORDER BY o.added_at, o.character_id) \
                     FILTER (WHERE o.last_error IS NOT NULL))[1] \
             FROM owners o LEFT JOIN names n ON n.id = o.corporation_id \
             LEFT JOIN owner_status s ON s.corporation_id = o.corporation_id \
             LEFT JOIN owner_settings w ON w.corporation_id = o.corporation_id \
             GROUP BY o.corporation_id, n.name, s.up, w.in_service_status ORDER BY 1"
        ),
        &[],
    )
    .map_err(|e| retry("admin notices: reading owners", e))?;
    for row in &rows.rows {
        let corporation = int(row, 0);
        let health = Health {
            structures: row.get(2).and_then(Db::as_bool).unwrap_or(false),
            notifications: row.get(3).and_then(Db::as_bool).unwrap_or(false),
            assets: row.get(4).and_then(Db::as_bool),
        };
        let up = health.up();
        let included = row.get(6).and_then(Db::as_bool).unwrap_or(true);
        let due = change(row.get(5).and_then(Db::as_bool), up)
            .filter(|_| settings.admin_notifications && included);
        if let Some(change) = due {
            // Past this run's notices: judged again next sync.
            if sent >= NOTICES_PER_RUN {
                continue;
            }
            let problem = row.get(7).and_then(Db::as_text);
            let (level, title, message) = notice(change, &text(row, 1), health, problem);
            sent += 1;
            if !send(&title, &message, level) {
                continue;
            }
        }
        // Stored whether or not a notice went, as aa-structures' is_up.
        storage::execute(
            "INSERT INTO owner_status (corporation_id, up, checked_at) VALUES ($1, $2, now()) \
             ON CONFLICT (corporation_id) DO UPDATE SET up = EXCLUDED.up, checked_at = now()",
            &[corporation.into(), up.into()],
        )
        .map_err(|e| retry("admin notices: storing status", e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_are_aa_structures() {
        assert_eq!(change(Some(true), false), Some(Change::Down));
        assert_eq!(change(Some(false), true), Some(Change::Restored));
        assert_eq!(change(None, true), Some(Change::Enabled));
        assert_eq!(change(None, false), None);
        assert_eq!(change(Some(true), true), None);
        assert_eq!(change(Some(false), false), None);
    }

    #[test]
    fn assets_count_once_read() {
        let health = |assets| Health {
            structures: true,
            notifications: true,
            assets,
        };
        assert!(health(None).up());
        assert!(health(Some(true)).up());
        assert!(!health(Some(false)).up());
    }

    #[test]
    fn the_down_notice_is_one_line_within_the_limits() {
        let health = Health {
            structures: true,
            notifications: false,
            assets: None,
        };
        let (level, title, message) = notice(
            Change::Down,
            "Otherworld Enterprises",
            health,
            Some(&"ESI said 403 ".repeat(200)),
        );
        assert!(matches!(level, Level::Danger));
        assert_eq!(title, "Services are down for Otherworld Enterprises");
        assert!(
            message.starts_with(
                "Structure services for Otherworld Enterprises are down. Admin action is likely \
                 required to restore services. Structures: up; notifications: down; assets: not \
                 read (they need the Director role). Last problem: ESI said 403"
            ),
            "{message}"
        );
        assert!(!message.contains('\n'));
        assert!(message.chars().count() <= notify::MAX_MESSAGE);
        let (_, _, restored) = notice(Change::Restored, "A", health, None);
        assert_eq!(restored, "Structure services for A have been restored.");
    }

    #[test]
    fn owners_added_read_as_aa_structures() {
        assert_eq!(
            added("Otherworld Enterprises", "Chribba", 1, 1, true),
            (
                "Structure owner added: Otherworld Enterprises".to_owned(),
                "Otherworld Enterprises was added as a new structure owner, with Chribba as its \
                 data source."
                    .to_owned()
            )
        );
        assert_eq!(
            added("Otherworld Enterprises", "Chribba Alt", 1, 2, false).1,
            "Chribba Alt was added as a data source for Otherworld Enterprises. It now has 2 \
             data sources."
        );
    }
}
