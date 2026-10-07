//! Buyback: aa-buybackprogram 1:1 (Jay, 2026-10-07).
//!
//! - **Programs** (`basic_access`; public ones for everyone signed in):
//!   pilots paste items from their inventory into a program's calculator
//!   and get a price, a tracking number and instructions for the
//!   contract to its manager (its character or corporation).
//! - **Tracking**: every 30 minutes the managers' contracts are read and
//!   each one titled with a tracking number is checked against its
//!   calculation (items, price, location, receiver, title) and flagged;
//!   contracts with a buyback prefix but no tracking are flagged as
//!   possible scams. Managers hear of new contracts, sellers of accepted
//!   or rejected ones (Tether's notifications), and a program's channel
//!   gets a card.
//! - **Statistics**: a pilot's own, a manager's programs', everyone's;
//!   leaderboards and performance by month, with a CSV export.
//! - **Reverse buyback**: members buy from a corporation hangar's stock,
//!   read from its assets, with reservations until their contract comes.
//!
//! Prices come from Fuzzwork or Janice (Settings), item data from
//! Tether's built-in static data (`sde-*`), "NPC" prices from ESI's
//! averages. A program's manager is one of the app's data sources.

mod calculator;
mod manage;
mod pages;
mod paste;
mod prices;
mod pricing;
mod programs;
mod reverse;
mod statics;
mod stats;
mod sync;

use chrono::{DateTime, Utc};
use tether_plugin_sdk::identity::{self, Viewer};
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{Page, PageError, Plugin, Request, Submission, SubmitResult};

/// Data sources' and statics' calls are made as nobody in particular.
pub(crate) const PUBLIC: tether_plugin_sdk::esi::Subject =
    tether_plugin_sdk::esi::Subject::Character(0);

struct Buyback;

impl Plugin for Buyback {
    fn render(request: Request) -> Result<Page, PageError> {
        let access = Access::current()?;
        pages::render(&access, &request)
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let access = Access::current()?;
        pages::submit(&access, &submission)
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "contracts" => sync::contracts(),
            "prices" => prices::refresh_all(),
            "wallets" => sync::wallets(),
            "hangars" => reverse::sync_all(&job),
            sync::RELAY => sync::relay(),
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(Buyback);

// ---- who is looking --------------------------------------------------------

/// The viewer, what they may do, and who they are for program
/// restrictions.
pub(crate) struct Access {
    pub viewer: Viewer,
    pub group_ids: Vec<i64>,
}

impl Access {
    fn current() -> Result<Self, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let group_ids = identity::groups().into_iter().map(|g| g.id).collect();
        Ok(Self { viewer, group_ids })
    }

    pub fn can(&self, permission: &str) -> bool {
        self.viewer.can(permission)
    }

    pub fn basic(&self) -> bool {
        self.can("basic_access")
    }

    /// AA's `can_manage_program_test`.
    pub fn manager(&self) -> bool {
        self.can("manage_programs") || self.can("manage_all_programs")
    }

    pub fn manage_all(&self) -> bool {
        self.can("manage_all_programs")
    }

    pub fn account(&self) -> i64 {
        self.viewer.account_id
    }

    /// The viewer's characters' ids.
    pub fn character_ids(&self) -> Vec<i64> {
        self.viewer.characters.iter().map(|c| c.id).collect()
    }

    /// The viewer's characters' corporations.
    pub fn corporation_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .viewer
            .characters
            .iter()
            .map(|c| c.corporation_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Whether a program managed by `manager_account` through
    /// `owner_character` is the viewer's to edit (AA's `user_can_manage`):
    /// theirs, or they manage every program.
    pub fn manages(&self, manager_account: i64, owner_character: i64) -> bool {
        self.manage_all()
            || (self.can("manage_programs")
                && (manager_account == self.account()
                    || self.character_ids().contains(&owner_character)))
    }

    /// Whether the viewer may use a program with these restrictions (AA's
    /// visibility rule, which its calculator didn't enforce, B2): public
    /// for everyone signed in; else `basic_access` and, if restricted,
    /// in one of its groups and one of its states, or its manager.
    pub fn may_use(&self, p: &Restrictions) -> bool {
        if p.is_public {
            return true;
        }
        if !self.basic() {
            return false;
        }
        let own = p.manager_account == self.account();
        let group = p.groups.is_empty() || p.groups.iter().any(|g| self.group_ids.contains(g));
        let state = p.states.is_empty()
            || p.states
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&self.viewer.state.name));
        own || (group && state)
    }
}

/// Who may use a program.
pub(crate) struct Restrictions {
    pub is_public: bool,
    pub manager_account: i64,
    pub groups: Vec<i64>,
    pub states: Vec<String>,
}

// ---- settings --------------------------------------------------------------

/// aa-buybackprogram's settings.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub price_method: String,
    pub price_source_id: i64,
    pub price_source_name: String,
    pub instant_prices: bool,
    pub price_age_warning_hours: i64,
    pub purge_hours: i64,
    pub track_prefill_contracts: bool,
    pub tracking_prefill: String,
    pub show_location_count: i64,
    pub disallow_any_disallowed: bool,
    pub reverse_enabled: bool,
    pub restrict_tracking_details: bool,
    pub prices_updated_at: Option<DateTime<Utc>>,
    pub sync_error: Option<String>,
}

pub(crate) fn settings() -> Result<Settings, storage::Error> {
    let rows = storage::query(
        "SELECT price_method, price_source_id, price_source_name, instant_prices, \
                price_age_warning_hours, purge_hours, track_prefill_contracts, tracking_prefill, \
                show_location_count, disallow_any_disallowed, reverse_enabled, \
                restrict_tracking_details, prices_updated_at, sync_error \
         FROM settings WHERE id = 1",
        &[],
    )?;
    let r = rows.rows.first().cloned().unwrap_or_default();
    Ok(Settings {
        price_method: opt_text(&r, 0).unwrap_or_else(|| "Fuzzwork".to_owned()),
        price_source_id: opt_int(&r, 1).unwrap_or(60003760),
        price_source_name: opt_text(&r, 2).unwrap_or_else(|| "Jita".to_owned()),
        instant_prices: boolean(&r, 3),
        price_age_warning_hours: opt_int(&r, 4).unwrap_or(48),
        purge_hours: opt_int(&r, 5).unwrap_or(48),
        track_prefill_contracts: r.get(6).and_then(Db::as_bool).unwrap_or(true),
        tracking_prefill: opt_text(&r, 7).unwrap_or_else(|| "aa-bbp".to_owned()),
        show_location_count: opt_int(&r, 8).unwrap_or(4),
        disallow_any_disallowed: boolean(&r, 9),
        reverse_enabled: r.get(10).and_then(Db::as_bool).unwrap_or(true),
        restrict_tracking_details: boolean(&r, 11),
        prices_updated_at: when(&r, 12),
        sync_error: opt_text(&r, 13),
    })
}

/// Whether the account turned off its accepted and rejected notices.
pub(crate) fn notifications_off(account: i64) -> bool {
    storage::query(
        "SELECT disable_notifications FROM user_settings WHERE account_id = $1",
        &[account.into()],
    )
    .ok()
    .and_then(|r| r.rows.first().map(|row| boolean(row, 0)))
    .unwrap_or(false)
}

// ---- helpers ---------------------------------------------------------------

pub(crate) fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

pub(crate) fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

pub(crate) fn int(row: &[Db], i: usize) -> i64 {
    row.get(i).and_then(Db::as_integer).unwrap_or_default()
}

pub(crate) fn opt_int(row: &[Db], i: usize) -> Option<i64> {
    row.get(i).and_then(Db::as_integer)
}

pub(crate) fn float(row: &[Db], i: usize) -> f64 {
    row.get(i).and_then(Db::as_float).unwrap_or_default()
}

pub(crate) fn boolean(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or(false)
}

pub(crate) fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn opt_text(row: &[Db], i: usize) -> Option<String> {
    row.get(i).and_then(Db::as_text).map(str::to_owned)
}

pub(crate) fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i)
        .and_then(Db::as_text)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub(crate) fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(crate) fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// A JSON array of numbers, for `= ANY(...)` through `jsonb`.
pub(crate) fn json_ids(ids: &[i64]) -> Db {
    Db::json(serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned()))
}

/// ISK as aa-buybackprogram writes it: "1 234 567.89".
pub(crate) fn isk_text(v: f64) -> String {
    let cents = (v * 100.0).round() as i64;
    let (whole, frac) = (cents.unsigned_abs() / 100, cents.unsigned_abs() % 100);
    let digits = whole.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    format!("{}{out}.{frac:02}", if cents < 0 { "-" } else { "" })
}

/// Rows a table shows (the host's limit).
pub(crate) const TABLE_ROWS: usize = 500;

/// Discord markdown out of names players choose.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let text = text.replace("://", ":\u{200B}//");
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '@' | '<'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::isk_text;

    #[test]
    fn isk_reads_as_aa_writes_it() {
        assert_eq!(isk_text(1_234_567.891), "1 234 567.89");
        assert_eq!(isk_text(0.5), "0.50");
        assert_eq!(isk_text(-1000.0), "-1 000.00");
    }
}
