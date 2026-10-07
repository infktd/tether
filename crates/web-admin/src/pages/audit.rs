//! The audit log (`/admin/audit`): every admin action and access change,
//! newest first, filtered by who, action, app and date on the toolbar
//! (all in the address), searched, paged, and exported as CSV
//! (`/admin/audit.csv`, the same filters, for the same permission).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{Days, NaiveDate, Utc};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::json;
use tether_core::permissions::ADMIN_AUDIT;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor, Entry, Filter, Who};
use tether_web_core::pages::toolbar::{FilterView, ListQuery, RangeView, TabLink, ToolbarView};
use tether_web_core::plugin_downloads::csv_line;

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

const PATH: &str = "/admin/audit";
const CSV_PATH: &str = "/admin/audit.csv";
/// Entries on a page.
const PAGE: i64 = 50;
/// Entries the CSV reads at a time.
const CSV_CHUNK: i64 = 1_000;
/// Pilots offered under Who: those behind the newest entries.
const WHO_CHOICES: i64 = 12;
/// The longest action or app a filter takes.
const MAX_NAME: usize = 100;

#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
    #[serde(default)]
    q: String,
    /// An account id, `system` or `cli`.
    #[serde(default)]
    who: String,
    /// An action (`group.join`), or a family of them (`group.*`).
    #[serde(default)]
    action: String,
    /// An app's id.
    #[serde(default)]
    app: String,
    /// `YYYY-MM-DD`, EVE time, both days included.
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
}

/// The filters asked for, those that make sense: the list's address and
/// what the log is filtered by.
struct Asked {
    list: ListQuery,
    filter: Filter,
    who: Option<Who>,
    action: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

/// A day the log could hold (years 1 to 9999; Postgres takes no earlier
/// than 4713 BC, chrono parses far beyond).
fn day(text: &str) -> Option<NaiveDate> {
    use chrono::Datelike;
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .ok()
        .filter(|d| (1..=9999).contains(&d.year()))
}

fn plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'*'))
}

/// Reads the query: what doesn't make sense is left out (of the filter
/// and the address alike). `actions` is every action the log holds, for
/// a family's.
fn asked(query: &AuditQuery, actions: &[String], path: &str) -> Asked {
    let who = match query.who.trim() {
        "system" => Some(Who::System),
        "cli" => Some(Who::Cli),
        other => other
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(|id| Who::Account(AccountId(id))),
    };
    let action = Some(query.action.trim()).filter(|a| plain_name(a));
    let wanted = action.map(|a| match a.strip_suffix(".*") {
        Some(family) => actions
            .iter()
            .filter(|x| x.split('.').next() == Some(family))
            .cloned()
            .collect(),
        None => vec![a.to_owned()],
    });
    let app = Some(query.app.trim())
        .filter(|a| plain_name(a) && !a.contains('*'))
        .map(str::to_owned);
    let (mut from, mut to) = (day(&query.from), day(&query.to));
    if let (Some(f), Some(t)) = (from, to)
        && f > t
    {
        (from, to) = (Some(t), Some(f));
    }
    let q: String = query.q.trim().chars().take(200).collect();
    let who_param = match who {
        None => String::new(),
        Some(Who::System) => "system".to_owned(),
        Some(Who::Cli) => "cli".to_owned(),
        Some(Who::Account(a)) => a.0.to_string(),
    };
    let date = |d: Option<NaiveDate>| d.map(|d| d.to_string()).unwrap_or_default();
    let list = ListQuery::new(path)
        .param("q", &q)
        .param("who", &who_param)
        .param("action", action.unwrap_or(""))
        .param("app", app.as_deref().unwrap_or(""))
        .param("from", &date(from))
        .param("to", &date(to));
    let midnight = |d: NaiveDate| d.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    let filter = Filter {
        who,
        actions: wanted,
        app,
        from: from.and_then(midnight),
        until: to
            .and_then(|t| t.checked_add_days(Days::new(1)))
            .and_then(midnight),
        words: list.words(),
    };
    Asked {
        list,
        filter,
        who,
        action: action.map(str::to_owned),
        from,
        to,
    }
}

pub struct AuditRow {
    pub at: String,
    pub actor: String,
    /// The log filtered by who did it.
    pub actor_href: String,
    pub action: String,
    pub action_href: String,
    pub target: String,
    pub details: String,
}

#[derive(Template)]
#[template(path = "admin_audit.html")]
struct AuditPage {
    shell: Shell,
    toolbar: ToolbarView,
    rows: Vec<AuditRow>,
    /// The next (older) page, and the newest, when this isn't it.
    older: Option<String>,
    newest: Option<String>,
    /// What the list says when it's empty.
    empty: String,
}

fn who_did(e: &Entry) -> String {
    match (e.actor_account_id, e.actor_name.as_deref()) {
        (None, Some("cli")) => "CLI".to_owned(),
        (None, _) => "System".to_owned(),
        (Some(id), name) => name.map_or_else(|| format!("Account {id}"), str::to_owned),
    }
}

fn details_text(e: &Entry) -> String {
    if e.details.as_object().is_some_and(|o| o.is_empty()) {
        String::new()
    } else {
        e.details.to_string()
    }
}

/// `GET /admin/audit`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<AuditQuery>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_AUDIT, "audit").await?;
    let actions = audit::actions(&state.db).await?;
    let asked = asked(&query, &actions, PATH);
    let list = &asked.list;
    let entries = audit::find(&state.db, &asked.filter, PAGE + 1, query.before).await?;
    let more = entries.len() as i64 > PAGE;
    let shown: Vec<Entry> = entries.into_iter().take(PAGE as usize).collect();
    let older = more
        .then(|| {
            shown
                .last()
                .map(|e| list.and(&[("before", &e.id.to_string())]))
        })
        .flatten();
    let rows = shown
        .iter()
        .map(|e| AuditRow {
            at: e.at.format("%Y-%m-%d %H:%M:%S").to_string(),
            actor: who_did(e),
            actor_href: list.with(
                "who",
                Some(&match (e.actor_account_id, e.actor_name.as_deref()) {
                    (Some(id), _) => id.to_string(),
                    (None, Some("cli")) => "cli".to_owned(),
                    (None, _) => "system".to_owned(),
                }),
            ),
            action: e.action.clone(),
            action_href: list.with("action", Some(&e.action)),
            target: e.target.clone().unwrap_or_default(),
            details: details_text(e),
        })
        .collect();
    let toolbar = toolbar(&state, &asked, &actions).await?;
    let filtered = !toolbar.chips.is_empty() || !toolbar.q.is_empty();
    Ok(render(
        StatusCode::OK,
        &AuditPage {
            shell,
            toolbar,
            rows,
            older,
            newest: query.before.map(|_| list.href()),
            empty: if filtered {
                "Nothing matches these filters.".to_owned()
            } else {
                "Nothing here yet.".to_owned()
            },
        },
    ))
}

/// The toolbar: the search, Who (the pilots behind the newest entries,
/// System and the CLI; any other pilot as a chip, chosen from a row),
/// Action (by family; one action as a chip, chosen from a row), App (those
/// the log has entries about), Date (recent days, or two dates) and the
/// CSV of it all.
async fn toolbar(
    state: &AppState,
    asked: &Asked,
    actions: &[String],
) -> Result<ToolbarView, AppError> {
    let list = &asked.list;
    let recent = audit::recent_actors(&state.db, WHO_CHOICES).await?;
    let mut who_choices: Vec<(String, String)> = recent
        .iter()
        .map(|(id, name)| (id.to_string(), name.clone()))
        .collect();
    who_choices.push(("system".to_owned(), "System".to_owned()));
    who_choices.push(("cli".to_owned(), "CLI".to_owned()));
    let mut t = ToolbarView::new(list)
        .search("Search who, action, target or details")
        .filter(list, "Who", "who", who_choices.clone());
    if let Some(Who::Account(a)) = asked.who
        && !recent.iter().any(|(id, _)| *id == a.0)
    {
        let name = audit::actor_name(&state.db, a)
            .await?
            .unwrap_or_else(|| format!("Account {}", a.0));
        t = t.chip(list, "Who", name, &["who"]);
    }
    let mut families: Vec<&str> = actions.iter().filter_map(|a| a.split('.').next()).collect();
    families.dedup();
    t = t.filter(
        list,
        "Action",
        "action",
        families.iter().map(|f| (format!("{f}.*"), (*f).to_owned())),
    );
    if let Some(action) = &asked.action
        && !action.ends_with(".*")
    {
        t = t.chip(list, "Action", action.clone(), &["action"]);
    }
    let names: std::collections::HashMap<String, String> = tether_db::plugins::list(&state.db)
        .await?
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect();
    let mut apps: Vec<(String, String)> = audit::apps(&state.db)
        .await?
        .into_iter()
        .map(|id| {
            let name = names.get(&id).cloned().unwrap_or_else(|| id.clone());
            (id, name)
        })
        .collect();
    apps.sort_by_key(|(_, name)| name.to_lowercase());
    let app = list.get("app").to_owned();
    if !app.is_empty() && !apps.iter().any(|(id, _)| *id == app) {
        let name = names.get(&app).cloned().unwrap_or_else(|| app.clone());
        t = t.chip(list, "App", name, &["app"]);
    }
    t = t.filter(list, "App", "app", apps);
    // Date: a chip for the range chosen; recent days, or two dates.
    let dates = |d: Option<NaiveDate>| d.map(|d| d.to_string());
    let range = match (dates(asked.from), dates(asked.to)) {
        (Some(f), Some(t)) if f == t => Some(f),
        (Some(f), Some(t)) => Some(format!("{f} to {t}")),
        (Some(f), None) => Some(format!("from {f}")),
        (None, Some(t)) => Some(format!("until {t}")),
        (None, None) => None,
    };
    if let Some(range) = range {
        t = t.chip(list, "Date", range, &["from", "to"]);
    }
    let today = Utc::now().date_naive();
    let base = list.without(&["from", "to"]);
    let since = |days: u64| {
        let from = today.checked_sub_days(Days::new(days)).unwrap_or(today);
        let current = asked.from == Some(from) && asked.to.is_none();
        let sep = if base.contains('?') { '&' } else { '?' };
        (format!("{base}{sep}from={from}"), current)
    };
    let choices = [
        ("Today", 0),
        ("Last 7 days", 6),
        ("Last 30 days", 29),
        ("Last 90 days", 89),
    ]
    .into_iter()
    .map(|(label, days)| {
        let (href, current) = since(days);
        TabLink {
            label: label.to_owned(),
            href,
            current,
        }
    })
    .collect();
    t.filters.push(FilterView {
        label: "Date (EVE)".to_owned(),
        choices,
        range: Some(RangeView {
            action: list.path.clone(),
            keep: list.keep(&["from", "to"]),
            from: (
                "from".to_owned(),
                asked.from.map(|d| d.to_string()).unwrap_or_default(),
            ),
            to: (
                "to".to_owned(),
                asked.to.map(|d| d.to_string()).unwrap_or_default(),
            ),
        }),
    });
    Ok(t.csv(list.at(CSV_PATH).href()))
}

/// `GET /admin/audit.csv`: the entries the same filters keep, every one
/// of them, newest first, read a thousand at a time as they're sent. For
/// those who may read the log; the export itself is logged.
pub async fn csv(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(&state, ADMIN_AUDIT).await?;
    let actions = audit::actions(&state.db).await?;
    let asked = asked(&query, &actions, CSV_PATH);
    // Asked by htmx (a link the browser didn't take as a download): back
    // as a navigation, so the browser saves the file, read once.
    if super::is_htmx(&headers) {
        let mut response = StatusCode::OK.into_response();
        if let Ok(value) = HeaderValue::from_str(&asked.list.href()) {
            response.headers_mut().insert("hx-redirect", value);
        }
        return Ok(response);
    }
    // A link from another site doesn't export in an admin's name: it opens
    // the log with those filters, to export from there.
    let cross_site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| site != "same-origin" && site != "none");
    if cross_site {
        return Ok(Redirect::to(&asked.list.at(PATH).href()).into_response());
    }
    if let Err(retry) = state
        .limits
        .audit_exports
        .check(session.account.0, std::time::Instant::now())
    {
        return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
    }
    audit::record(
        &state.db,
        Actor::Account(session.account),
        "audit.export",
        None,
        json!({ "filters": asked.list.keep(&[]).into_iter().collect::<std::collections::BTreeMap<_, _>>() }),
    )
    .await
    .map_err(AppError::from)?;
    let name = format!("tether-audit-{}.csv", Utc::now().format("%Y%m%d-%H%M"));
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        axum::body::Body::from_stream(Csv::new(state.db.clone(), asked.filter)),
    )
        .into_response())
}

/// The CSV's columns.
const COLUMNS: [&str; 7] = [
    "id",
    "at (EVE)",
    "who",
    "account id",
    "action",
    "target",
    "details",
];

fn csv_row(e: &Entry) -> String {
    csv_line(&[
        e.id.to_string(),
        e.at.format("%Y-%m-%d %H:%M:%S").to_string(),
        who_did(e),
        e.actor_account_id
            .map(|a| a.to_string())
            .unwrap_or_default(),
        e.action.clone(),
        e.target.clone().unwrap_or_default(),
        details_text(e),
    ])
}

type Step = Pin<Box<dyn Future<Output = Option<(Result<String, std::io::Error>, Reading)>> + Send>>;

/// The CSV as it's sent: its head, then each thousand entries.
struct Csv {
    step: Option<Step>,
}

struct Reading {
    db: PgPool,
    filter: Filter,
    /// The last id sent; `None` before the first chunk.
    before: Option<i64>,
    started: bool,
    done: bool,
}

impl Csv {
    fn new(db: PgPool, filter: Filter) -> Self {
        let reading = Reading {
            db,
            filter,
            before: None,
            started: false,
            done: false,
        };
        Self {
            step: Some(Box::pin(next_chunk(reading))),
        }
    }
}

impl Stream for Csv {
    type Item = Result<String, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(step) = self.step.as_mut() else {
            return Poll::Ready(None);
        };
        match step.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.step = None;
                Poll::Ready(None)
            }
            Poll::Ready(Some((Err(err), _))) => {
                self.step = None;
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(Some((chunk, reading))) => {
                self.step = Some(Box::pin(next_chunk(reading)));
                Poll::Ready(Some(chunk))
            }
        }
    }
}

async fn next_chunk(mut r: Reading) -> Option<(Result<String, std::io::Error>, Reading)> {
    if r.done {
        return None;
    }
    let mut out = String::new();
    if !r.started {
        r.started = true;
        out.push_str(&csv_line(&COLUMNS.map(str::to_owned)));
    }
    match audit::find(&r.db, &r.filter, CSV_CHUNK, r.before).await {
        Ok(entries) => {
            r.done = (entries.len() as i64) < CSV_CHUNK;
            r.before = entries.last().map(|e| e.id).or(r.before);
            for e in &entries {
                out.push_str(&csv_row(e));
            }
            Some((Ok(out), r))
        }
        Err(err) => {
            // The file ends short; the browser shows the download failed.
            tracing::error!(error = %err, "audit log CSV stopped");
            r.done = true;
            Some((
                Err(std::io::Error::other("the audit log couldn't be read")),
                r,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(pairs: &[(&str, &str)]) -> AuditQuery {
        let mut q = AuditQuery::default();
        for (k, v) in pairs {
            let v = (*v).to_owned();
            match *k {
                "q" => q.q = v,
                "who" => q.who = v,
                "action" => q.action = v,
                "app" => q.app = v,
                "from" => q.from = v,
                "to" => q.to = v,
                _ => {}
            }
        }
        q
    }

    #[test]
    fn filters_that_make_no_sense_are_left_out() {
        let actions = vec![
            "group.join".to_owned(),
            "group.leave".to_owned(),
            "plugin.installed".to_owned(),
        ];
        let a = asked(
            &query(&[
                ("who", "nobody"),
                ("action", "group.*"),
                ("app", "acme moons"),
                ("from", "2026-10-07"),
                ("to", "2026-10-01"),
            ]),
            &actions,
            PATH,
        );
        assert_eq!(a.filter.who, None);
        assert_eq!(
            a.filter.actions,
            Some(vec!["group.join".to_owned(), "group.leave".to_owned()])
        );
        assert_eq!(a.filter.app, None);
        // The dates the right way round, the last day included.
        assert_eq!(
            a.list.href(),
            "/admin/audit?action=group.%2A&from=2026-10-01&to=2026-10-07"
        );
        assert_eq!(
            a.filter.until.map(|t| t.to_rfc3339()),
            Some("2026-10-08T00:00:00+00:00".to_owned())
        );
        let b = asked(
            &query(&[
                ("who", "7"),
                ("action", "plugin.installed"),
                ("app", "acme.moons"),
            ]),
            &actions,
            PATH,
        );
        assert_eq!(b.filter.who, Some(Who::Account(AccountId(7))));
        assert_eq!(b.filter.actions, Some(vec!["plugin.installed".to_owned()]));
        assert_eq!(b.filter.app.as_deref(), Some("acme.moons"));
        // A family nothing in the log is in finds nothing.
        let c = asked(&query(&[("action", "nothing.*")]), &actions, PATH);
        assert_eq!(c.filter.actions, Some(Vec::new()));
    }
}
