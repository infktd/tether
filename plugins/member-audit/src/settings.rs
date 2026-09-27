//! aa-memberaudit's settings, on the app's Settings page (for `manage`)
//! rather than in the server's configuration: data retention, mails kept,
//! corporation roles, and the sharing timeout. AA's defaults.

use tether_plugin_sdk::identity::Viewer;
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{Field, Form, Page, PageError, Submission, SubmitResult, log};

use crate::failed;

/// `MEMBERAUDIT_DATA_RETENTION_LIMIT`: days mail, contracts and wallet
/// history are kept, at least 7.
pub(crate) const RETENTION_DAYS: (i64, i64, i64) = (7, 360, 3650);
/// `MEMBERAUDIT_MAX_MAILS`: mails kept per character.
pub(crate) const MAX_MAILS: (i64, i64, i64) = (1, 250, 5000);
/// `MEMBERAUDIT_SHARING_TIMEOUT`, in minutes (0: until unshared). A year
/// at most.
pub(crate) const SHARING_TIMEOUT: (i64, i64, i64) = (0, 0, 525_600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settings {
    pub retention_days: i64,
    pub max_mails: i64,
    /// `MEMBERAUDIT_FEATURE_ROLES_ENABLED`: read and show corporation
    /// roles. Off by default.
    pub roles: bool,
    pub sharing_timeout_minutes: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            retention_days: RETENTION_DAYS.1,
            max_mails: MAX_MAILS.1,
            roles: false,
            sharing_timeout_minutes: SHARING_TIMEOUT.1,
        }
    }
}

/// The settings, or AA's defaults where none are stored.
pub(crate) fn get() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT retention_days, max_mails, roles_enabled, sharing_timeout_minutes \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let defaults = Settings::default();
    let Some(row) = rows.rows.first() else {
        return Ok(defaults);
    };
    let number = |i: usize, fallback: i64| row.get(i).and_then(Db::as_integer).unwrap_or(fallback);
    Ok(Settings {
        retention_days: number(0, defaults.retention_days),
        max_mails: number(1, defaults.max_mails),
        roles: row.get(2).and_then(Db::as_bool).unwrap_or(defaults.roles),
        sharing_timeout_minutes: number(3, defaults.sharing_timeout_minutes),
    })
}

/// The settings for a page.
pub(crate) fn for_page() -> Result<Settings, PageError> {
    get().map_err(|e| failed("reading settings", e))
}

fn range((min, _, max): (i64, i64, i64)) -> (Option<f64>, Option<f64>) {
    // Exact as f64: every bound is far below 2^53.
    #[allow(clippy::cast_precision_loss)]
    (Some(min as f64), Some(max as f64))
}

pub(crate) fn page(note: Option<&str>) -> Result<Page, PageError> {
    let settings = for_page()?;
    let (retention_min, retention_max) = range(RETENTION_DAYS);
    let (mails_min, mails_max) = range(MAX_MAILS);
    let (share_min, share_max) = range(SHARING_TIMEOUT);
    let mut page = Page::new("Member Audit settings")
        .description("aa-memberaudit's settings, with its defaults")
        .link("My Characters", "");
    if let Some(note) = note {
        page = page.text(note);
    }
    Ok(page.form(
        Form::new("settings", "Save")
            .field(
                Field::number(
                    "retention_days",
                    "Days to keep mail, contracts and wallet history",
                )
                .range(retention_min, retention_max, true)
                .value(settings.retention_days.to_string())
                .help("MEMBERAUDIT_DATA_RETENTION_LIMIT: 360, and at least 7")
                .required(),
            )
            .field(
                Field::number("max_mails", "Mails kept per character")
                    .range(mails_min, mails_max, true)
                    .value(settings.max_mails.to_string())
                    .help("MEMBERAUDIT_MAX_MAILS: 250. The newest are kept")
                    .required(),
            )
            .field(
                Field::checkbox(
                    "roles_enabled",
                    "Read and show corporation roles",
                    settings.roles,
                )
                .help(
                    "MEMBERAUDIT_FEATURE_ROLES_ENABLED: off. Turning it off forgets the roles read",
                ),
            )
            .field(
                Field::number(
                    "sharing_timeout_minutes",
                    "Minutes a character stays shared",
                )
                .range(share_min, share_max, true)
                .value(settings.sharing_timeout_minutes.to_string())
                .help("MEMBERAUDIT_SHARING_TIMEOUT: 0, shared until its pilot stops sharing it")
                .required(),
            ),
    ))
}

fn whole(submission: &Submission, name: &str, (min, _, max): (i64, i64, i64)) -> Option<i64> {
    submission
        .value(name)
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|n| (min..=max).contains(n))
}

pub(crate) fn save(viewer: &Viewer, submission: &Submission) -> Result<SubmitResult, PageError> {
    let (Some(retention), Some(mails), Some(sharing)) = (
        whole(submission, "retention_days", RETENTION_DAYS),
        whole(submission, "max_mails", MAX_MAILS),
        whole(submission, "sharing_timeout_minutes", SHARING_TIMEOUT),
    ) else {
        return Ok(SubmitResult::Page(page(Some(
            "Each number must be a whole number within its range.",
        ))?));
    };
    let roles = submission.checked("roles_enabled");
    let mut statements = vec![storage::Statement::new(
        "UPDATE settings SET retention_days = $1, max_mails = $2, roles_enabled = $3, \
         sharing_timeout_minutes = $4 WHERE id = 1",
        vec![retention.into(), mails.into(), roles.into(), sharing.into()],
    )];
    if !roles {
        // Off: what was read goes, and is read afresh if it's turned on.
        statements.push(storage::Statement::new("DELETE FROM roles", vec![]));
        statements.push(storage::Statement::new(
            "DELETE FROM section_syncs WHERE section = 'roles'",
            vec![],
        ));
    }
    storage::transaction(&statements).map_err(|e| failed("saving settings", e))?;
    // Admins see who changed what in the plugin's log.
    log::info(format!(
        "settings changed by {} ({}): keep {retention} days, {mails} mails, roles {roles}, \
         sharing timeout {sharing} min",
        viewer.main.name, viewer.main.id
    ));
    Ok(SubmitResult::Redirect("settings".into()))
}
