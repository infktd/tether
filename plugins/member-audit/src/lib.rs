//! Member Audit (Alliance Auth's name for it, after aa-memberaudit; PRD
//! F20).
//!
//! - **My Characters**: a card per character (portrait, logos, location,
//!   ship, wallet, skill points, training, last update) after Tether's
//!   Register Character card, with combined totals for multiboxers. It is
//!   also the Dashboard's lead widget.
//! - **Character Sheet**: aa-memberaudit's tabs across a few pages:
//!   Overview (with corporation history, roles and titles, killmails),
//!   Skills (queue, skills by group, skill sets, attributes), Assets,
//!   Wallet (journal, transactions, market orders, contracts, loyalty),
//!   Clones (implants, jump clones), Industry (jobs, blueprints, mining,
//!   planets), Contacts (contacts, NPC standings), and Mail on its own
//!   audited pages. Pilots share their own characters from it.
//! - **Character Finder**: member characters within the viewer's scope,
//!   with each one's main, main organisation and state.
//! - **Skill Sets** and **Reports**.
//! - **Settings**: aa-memberaudit's settings (see `settings`).
//! - **Secure Groups filters**: a skill at a level, a skill set, an item.
//!
//! Who sees what follows aa-memberaudit's permissions (see `access`). Data
//! comes from the characters registered with Member Audit (its user
//! scopes) by pilots holding one of its permissions, in any state; `sync`
//! reads each section on its own clock within the host's ESI budget.

mod access;
mod exports;
mod filters;
mod mail;
mod pages;
mod sets;
mod settings;
mod sheet;
mod sync;

use chrono::{DateTime, SecondsFormat, Utc};
use tether_plugin_sdk::identity;
use tether_plugin_sdk::jobs::{Job, JobError};
use tether_plugin_sdk::storage::{self, Value as Db};
use tether_plugin_sdk::{Page, PageError, Plugin, Request, Submission, SubmitResult, Table, Value};

struct MemberAudit;

impl Plugin for MemberAudit {
    fn render(request: Request) -> Result<Page, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = access::Access::of(&viewer);
        let path = request.path.as_str();
        let parts: Vec<&str> = path.split('/').collect();
        match parts.as_slice() {
            [""] => pages::my_characters(&viewer),
            ["finder"] => pages::finder(&access, &request),
            ["skill-sets"] => sets::skill_sets_page(&access, None),
            ["reports"] => sets::reports(&access),
            ["data-export"] if viewer.can("exports_access") => exports::page(None),
            ["settings"] if viewer.can("manage") => settings::page(None),
            ["character", id, rest @ ..] => {
                let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
                sheet::render(&access, id, rest)
            }
            ["mail", id, rest @ ..] => {
                let id: i64 = id.parse().map_err(|_| PageError::NotFound)?;
                mail::render(&access, id, rest)
            }
            _ => Err(PageError::NotFound),
        }
    }

    fn submit(submission: Submission) -> Result<SubmitResult, PageError> {
        let viewer = identity::viewer().ok_or(PageError::Forbidden)?;
        let access = access::Access::of(&viewer);
        let path = submission.request.path.clone();
        match (path.as_str(), submission.form.as_str()) {
            ("skill-sets", "add_set") if viewer.can("manage") => {
                sets::add_set(&access, &submission)
            }
            ("data-export", "update_export") if viewer.can("exports_access") => {
                exports::update(submission.value("topic"))
            }
            ("settings", "settings") if viewer.can("manage") => {
                settings::save(&viewer, &submission)
            }
            ("skill-sets", "delete_set") if viewer.can("manage") => {
                sets::delete_set(&viewer, submission.value("set"))
            }
            ("finder", "search") if access.finder => Ok(SubmitResult::Page(pages::finder_page(
                &access,
                submission.value("q"),
            )?)),
            (_, "update_character") => sheet::update_now(&access, &submission),
            (_, "share_character") => sheet::share(&access, &submission, true),
            (_, "unshare_character") => sheet::share(&access, &submission, false),
            _ => Err(PageError::Forbidden),
        }
    }

    fn run_job(job: Job) -> Result<(), JobError> {
        match job.name.as_str() {
            "sync" | sync::MORE => sync::run(None),
            sync::UPDATE => {
                let character = payload_id(&job.payload, "character")
                    .ok_or_else(|| JobError::Permanent("no character".to_owned()))?;
                sync::run(Some(character))
            }
            "report_filters" => filters::report_filters(0),
            exports::JOB => exports::run(&job.payload),
            exports::SCHEDULE => exports::all(),
            filters::MORE_REPORTS => {
                filters::report_filters(payload_id(&job.payload, "from").unwrap_or(0) as usize)
            }
            other => Err(JobError::Permanent(format!("no job {other}"))),
        }
    }
}

tether_plugin_sdk::export!(MemberAudit);

// ---- helpers shared by the modules -------------------------------------------

/// A whole number from a job's JSON payload.
fn payload_id(payload: &str, key: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get(key)?
        .as_i64()
        .filter(|n| *n >= 0)
}

pub(crate) fn failed(what: &str, err: impl std::fmt::Debug) -> PageError {
    PageError::Failed(format!("{what}: {err:?}"))
}

pub(crate) fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

pub(crate) fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
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

pub(crate) fn opt_float(row: &[Db], i: usize) -> Option<f64> {
    row.get(i).and_then(Db::as_float)
}

pub(crate) fn text(row: &[Db], i: usize) -> String {
    row.get(i)
        .and_then(Db::as_text)
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn boolean(row: &[Db], i: usize) -> bool {
    row.get(i).and_then(Db::as_bool).unwrap_or_default()
}

pub(crate) fn when(row: &[Db], i: usize) -> Option<DateTime<Utc>> {
    row.get(i).and_then(Db::as_text).and_then(parse_time)
}

/// A time value, or empty.
pub(crate) fn time_or_blank(row: &[Db], i: usize) -> Value {
    when(row, i).map_or_else(|| "".into(), |t| tether_plugin_sdk::time(rfc3339(t)))
}

pub(crate) fn with_rows(mut table: Table, rows: impl IntoIterator<Item = Vec<Value>>) -> Table {
    for row in rows {
        table = table.row(row);
    }
    table
}

pub(crate) fn query(sql: &str, params: &[Db]) -> Result<Vec<Vec<Db>>, PageError> {
    storage::query(sql, params)
        .map(|r| r.rows)
        .map_err(|e| failed("reading", e))
}

/// A count as a page value.
pub(crate) fn count(n: usize) -> Value {
    Value::Number(i64::try_from(n).unwrap_or(i64::MAX))
}

/// A name for an id: stored, or the id.
const NAME: &str = "coalesce((SELECT name FROM names WHERE id = {}), {}::text)";

/// SQL naming the id in `column` (a fixed column name, never data).
pub(crate) fn name_of(column: &str) -> String {
    NAME.replacen("{}", column, 2)
}

/// Ids as a comma list, for `string_to_array($n, ',')::bigint[]`.
pub(crate) fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// EVE's text (mail bodies, bios, titles) as plain text: line breaks
/// kept, tags dropped, entities read.
pub(crate) fn plain_text(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            rest = &rest[start..];
            break;
        };
        let tag = rest[start + 1..start + end].trim().to_ascii_lowercase();
        if tag == "br" || tag == "br/" || tag == "br /" || tag.starts_with("/p") {
            out.push('\n');
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    // No control characters but line breaks and tabs.
    out.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect::<String>()
        .trim()
        .to_owned()
}

/// At most `max` characters of `text`.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eve_markup_reads_as_plain_text() {
        assert_eq!(
            plain_text("<font size=\"12\">Hi &amp; o7</font><br>Fly <b>safe</b>&lt;3"),
            "Hi & o7\nFly safe<3"
        );
        assert_eq!(plain_text("a < b"), "a < b");
        assert_eq!(plain_text("line\u{0}\u{7}\nnext"), "line\nnext");
    }

    #[test]
    fn clipping_counts_characters() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("ééééé", 3), "éé…");
    }

    #[test]
    fn payload_ids_are_whole_and_not_negative() {
        assert_eq!(
            payload_id(r#"{"character": 90000001}"#, "character"),
            Some(90_000_001)
        );
        assert_eq!(payload_id(r#"{"character": -1}"#, "character"), None);
        assert_eq!(payload_id("nope", "character"), None);
    }
}
