//! Plugin pages (F17): `/plugins/<id>/<path>`, drawn by the host from the
//! page the plugin describes, and forms posted back to it.
//!
//! Before the plugin is called: a signed-in account, a running plugin, a
//! path that is a link path, the page's permission from the manifest
//! (`[[pages]]`; admins only where no rule covers it), and a capped query
//! string. Anyone who may not open a page gets the same 404 as for a page
//! that doesn't exist. A posted form is checked against the form as the
//! plugin currently draws it, and rate-limited per account and plugin,
//! before `submit` sees it. What a plugin says went wrong goes to its log
//! for admins; users see a generic message.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use tether_plugins::host::{
    FieldKind, Page, PageError as PluginPageError, RenderError, Request, Section, Submission,
    SubmitResult, Tone, Value,
};
use tether_plugins::services::{Builtin, Character, State as ViewerState, Viewer};
use tether_plugins::{manifest, page as page_rules};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::plugin_jobs::record_logs;
use crate::plugins::{Running, page_href};

/// The query string a plugin page may get, in bytes and in pairs.
pub const MAX_QUERY_BYTES: usize = 2 * 1024;
pub const MAX_QUERY_PAIRS: usize = 20;
/// A posted form's body, and its fields.
pub const MAX_FORM_BYTES: usize = 64 * 1024;
const MAX_FORM_PAIRS: usize = 100;
/// The host's own query parameter: which tab to show.
const TAB: &str = "_tab";
/// The host's own form field: which form was posted.
const FORM: &str = "_form";

const FAILED: &str = "This page couldn't be shown. The app's admins can see why in its log.";
const MISSING: &str = "There's nothing at this address.";

fn missing() -> PageError {
    AppError::not_found(MISSING).into()
}

// ---- what the templates draw ------------------------------------------------

pub struct ValueView {
    pub text: String,
    /// The full value, shown on hover (ISK, times).
    pub title: Option<String>,
    pub href: Option<String>,
    /// A badge, and its Basecoat variant ("" for the default).
    pub badge: Option<&'static str>,
}

pub struct StatView {
    pub label: String,
    pub value: ValueView,
    pub caption: Option<String>,
}

pub struct ColumnView {
    pub label: String,
    pub numeric: bool,
}

pub struct TableView {
    pub title: Option<String>,
    pub columns: Vec<ColumnView>,
    pub rows: Vec<Vec<ValueView>>,
    pub empty: Option<String>,
}

pub struct CardView {
    pub title: String,
    pub description: Option<String>,
    pub fields: Vec<(String, ValueView)>,
}

pub struct ChoiceView {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

pub struct FieldView {
    pub name: String,
    pub label: String,
    pub help: Option<String>,
    pub required: bool,
    /// text, textarea, number, select or checkbox.
    pub kind: &'static str,
    pub value: String,
    pub max_length: u32,
    pub placeholder: String,
    pub min: String,
    pub max: String,
    pub step: &'static str,
    pub options: Vec<ChoiceView>,
    pub checked: bool,
}

pub struct FormView {
    pub id: String,
    pub action: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<FieldView>,
    pub submit_label: String,
}

pub enum SectionView {
    Stats(Vec<StatView>),
    Table(TableView),
    Card(CardView),
    Text(String),
    Form(FormView),
}

pub struct TabLink {
    pub label: String,
    pub href: String,
    pub current: bool,
}

/// `1.24b`, `350.2m`, `12.5k`: ISK in tables.
fn short_isk(amount: f64) -> String {
    let abs = amount.abs();
    let (scaled, unit) = if abs >= 1e12 {
        (amount / 1e12, "t")
    } else if abs >= 1e9 {
        (amount / 1e9, "b")
    } else if abs >= 1e6 {
        (amount / 1e6, "m")
    } else if abs >= 1e3 {
        (amount / 1e3, "k")
    } else {
        return format!("{amount:.0}");
    };
    let text = format!("{scaled:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}{unit}")
}

/// `1,240,000,000`.
fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn value(plugin: &str, value: &Value) -> ValueView {
    let plain = |text: String| ValueView {
        text,
        title: None,
        href: None,
        badge: None,
    };
    match value {
        Value::Text(text) => plain(text.clone()),
        Value::Number(n) => plain(grouped(*n)),
        Value::Isk(amount) => ValueView {
            title: Some(format!("{} ISK", grouped(amount.trunc() as i64))),
            ..plain(short_isk(*amount))
        },
        Value::Time(text) => {
            // Checked as RFC 3339 by the host already.
            let shown = chrono::DateTime::parse_from_rfc3339(text)
                .map(|at| {
                    at.with_timezone(&chrono::Utc)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|_| text.clone());
            ValueView {
                title: Some(format!("{text} (EVE time)")),
                ..plain(shown)
            }
        }
        Value::Badge(badge) => ValueView {
            badge: Some(match badge.tone {
                Tone::Neutral | Tone::Warning => "outline",
                Tone::Success => "secondary",
                Tone::Danger => "destructive",
                Tone::Accent => "",
            }),
            ..plain(badge.label.clone())
        },
        Value::Link(link) => ValueView {
            href: Some(page_href(plugin, &link.path)),
            ..plain(link.label.clone())
        },
    }
}

fn number_text(n: Option<f64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_default()
}

fn section(plugin: &str, action: &str, section: &Section) -> SectionView {
    match section {
        Section::Stats(stats) => SectionView::Stats(
            stats
                .iter()
                .map(|s| StatView {
                    label: s.label.clone(),
                    value: value(plugin, &s.value),
                    caption: s.caption.clone(),
                })
                .collect(),
        ),
        Section::Table(table) => SectionView::Table(TableView {
            title: table.title.clone(),
            columns: table
                .columns
                .iter()
                .map(|c| ColumnView {
                    label: c.label.clone(),
                    numeric: c.numeric,
                })
                .collect(),
            rows: table
                .rows
                .iter()
                .map(|row| row.iter().map(|v| value(plugin, v)).collect())
                .collect(),
            empty: table.empty.clone(),
        }),
        Section::Card(card) => SectionView::Card(CardView {
            title: card.title.clone(),
            description: card.description.clone(),
            fields: card
                .fields
                .iter()
                .map(|(label, v)| (label.clone(), value(plugin, v)))
                .collect(),
        }),
        Section::Text(text) => SectionView::Text(text.clone()),
        Section::Form(form) => SectionView::Form(FormView {
            id: form.id.clone(),
            action: action.to_owned(),
            title: form.title.clone(),
            description: form.description.clone(),
            submit_label: form.submit_label.clone(),
            fields: form
                .fields
                .iter()
                .map(|f| {
                    let mut view = FieldView {
                        name: f.name.clone(),
                        label: f.label.clone(),
                        help: f.help.clone(),
                        required: f.required,
                        kind: "text",
                        value: String::new(),
                        max_length: 0,
                        placeholder: String::new(),
                        min: String::new(),
                        max: String::new(),
                        step: "any",
                        options: Vec::new(),
                        checked: false,
                    };
                    match &f.kind {
                        FieldKind::Text(input) | FieldKind::Textarea(input) => {
                            view.kind = if matches!(f.kind, FieldKind::Text(_)) {
                                "text"
                            } else {
                                "textarea"
                            };
                            view.value = input.value.clone().unwrap_or_default();
                            view.max_length = input.max_length;
                            view.placeholder = input.placeholder.clone().unwrap_or_default();
                        }
                        FieldKind::Number(input) => {
                            view.kind = "number";
                            view.value = number_text(input.value);
                            view.min = number_text(input.min);
                            view.max = number_text(input.max);
                            view.step = if input.integer { "1" } else { "any" };
                        }
                        FieldKind::Select(input) => {
                            view.kind = "select";
                            view.options = input
                                .options
                                .iter()
                                .map(|c| ChoiceView {
                                    selected: input.value.as_deref() == Some(c.value.as_str()),
                                    value: c.value.clone(),
                                    label: c.label.clone(),
                                })
                                .collect();
                        }
                        FieldKind::Checkbox(checked) => {
                            view.kind = "checkbox";
                            view.checked = *checked;
                        }
                    }
                    view
                })
                .collect(),
        }),
    }
}

#[derive(Template)]
#[template(path = "plugin_page.html")]
struct PluginPage {
    shell: Shell,
    plugin_name: String,
    title: String,
    description: Option<String>,
    sections: Vec<SectionView>,
    tabs: Vec<TabLink>,
    tab_sections: Vec<SectionView>,
    error: Option<String>,
    watermark: String,
}

// ---- access -----------------------------------------------------------------

/// What a request for a plugin page has been checked into.
struct Opened {
    shell: Shell,
    running: Running,
    /// Who is looking, for `identity.current`.
    viewer: Viewer,
    path: String,
    query: Vec<(String, String)>,
    tab: usize,
    /// The page's own address, with its query: forms post back here.
    href: String,
}

/// Everything before the plugin is called. Anything that doesn't pass is
/// a 404, whether the page is missing or just not for this account.
async fn open(
    state: &AppState,
    session: Option<CurrentSession>,
    id: &str,
    path: &str,
    raw_query: Option<&str>,
) -> Result<(CurrentSession, Opened), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    manifest::check_id(id).map_err(|_| missing())?;
    let running = state.plugins.running(id).ok_or_else(missing)?;
    page_rules::check_link_path(path).map_err(|_| missing())?;
    let needed = running
        .manifest
        .page_permission(path)
        .unwrap_or_else(|| tether_core::permissions::ADMIN_PLUGINS.to_owned());
    let perms = tether_db::permissions::effective(&state.db, session.account).await?;
    if !perms.contains(&needed) {
        return Err(missing());
    }
    let viewer = viewer(state, &session, &running, &perms).await?;
    let raw = raw_query.unwrap_or("");
    if raw.len() > MAX_QUERY_BYTES {
        return Err(AppError::bad_request("That address is too long.").into());
    }
    let pairs: Vec<(String, String)> = Query::try_from_uri(
        &format!("/?{raw}")
            .parse()
            .map_err(|_| AppError::bad_request("That address isn't valid."))?,
    )
    .map(|Query(pairs)| pairs)
    .map_err(|_| AppError::bad_request("That address isn't valid."))?;
    if pairs.len() > MAX_QUERY_PAIRS {
        return Err(AppError::bad_request("That address is too long.").into());
    }
    let tab = pairs
        .iter()
        .find(|(k, _)| k == TAB)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let query: Vec<(String, String)> = pairs.into_iter().filter(|(k, _)| k != TAB).collect();
    // Not "plugins": that's the admin page; the plugin's own link is
    // marked through active_href.
    let mut shell = load(state, &session, "plugin-page").await?.shell;
    let href = page_href(id, path);
    shell.active_href = href.clone();
    let href = if raw.is_empty() {
        href
    } else {
        format!("{href}?{raw}")
    };
    Ok((
        session,
        Opened {
            shell,
            running,
            viewer,
            path: path.to_owned(),
            query,
            tab,
            href,
        },
    ))
}

/// The account as the plugin sees it: its characters (main first), state,
/// and which of this plugin's own permissions it holds.
async fn viewer(
    state: &AppState,
    session: &CurrentSession,
    running: &Running,
    perms: &std::collections::BTreeSet<String>,
) -> Result<Viewer, AppError> {
    let plugin = &running.manifest.plugin.id;
    let characters: Vec<Character> =
        tether_db::plugin_esi::account_characters(&state.db, session.account)
            .await?
            .into_iter()
            .map(|c| Character {
                id: c.id,
                name: c.name,
                corporation_id: c.corporation_id.unwrap_or(0),
                alliance_id: c.alliance_id,
            })
            .collect();
    // Plugins attribute and gate by the main: without one (sold, or its
    // token gone), there's nothing true to tell them.
    let main_id = tether_db::accounts::get(&state.db, session.account)
        .await?
        .and_then(|a| a.main)
        .map(|m| m.id)
        .ok_or_else(|| AppError::bad_request("Choose a main character first (Change Main)."))?;
    let main = characters
        .iter()
        .find(|c| c.id == main_id)
        .cloned()
        .ok_or_else(AppError::unauthorized)?;
    let current = tether_db::states::account_state(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let state_view = ViewerState {
        builtin: current.builtin.map(|b| match b {
            tether_core::states::Builtin::Member => Builtin::Member,
            tether_core::states::Builtin::Blue => Builtin::Blue,
            // Plugins know three states; the Blacklist holds nothing anyway.
            tether_core::states::Builtin::Guest | tether_core::states::Builtin::Blacklist => {
                Builtin::Guest
            }
        }),
        name: current.name,
    };
    let prefix = format!("plugin.{plugin}.");
    Ok(Viewer {
        account_id: session.account.0,
        main,
        characters,
        state: state_view,
        // Only names this plugin declares: ids can nest (nmu.esi and
        // nmu.esi.extra), so a prefix alone could leak another plugin's.
        permissions: running
            .manifest
            .permissions
            .keys()
            .filter(|name| perms.contains(&format!("{prefix}{name}")))
            .cloned()
            .collect(),
    })
}

/// Records a failure in the plugin's log (for admins) and answers users
/// with a generic page.
async fn failed(state: &AppState, opened: &Opened, what: &str) -> PageError {
    let id = &opened.running.manifest.plugin.id;
    let line = tether_plugins::host::LogRecord {
        level: tether_plugins::host::Level::Error,
        message: tether_plugins::host::printable(what, tether_plugins::host::MAX_LOG_TEXT),
    };
    record_logs(&state.db, id, &source(&opened.path), &[line]).await;
    AppError::new(StatusCode::INTERNAL_SERVER_ERROR, FAILED).into()
}

fn source(path: &str) -> String {
    format!("page:/{path}")
}

/// A plugin's answer, turned into what users see.
async fn render_error(state: &AppState, opened: &Opened, err: RenderError) -> PageError {
    match err {
        RenderError::Plugin(PluginPageError::NotFound) => missing(),
        RenderError::Plugin(PluginPageError::Forbidden) => AppError::forbidden().into(),
        RenderError::Plugin(PluginPageError::Failed(why)) => {
            failed(state, opened, &format!("the page failed: {why}")).await
        }
        RenderError::Call(call) => failed(state, opened, &format!("the page failed: {call}")).await,
        RenderError::Invalid(problem) => {
            failed(state, opened, &format!("the page is invalid: {problem}")).await
        }
    }
}

async fn render_page(state: &AppState, opened: &Opened) -> Result<Page, PageError> {
    let request = Request {
        path: opened.path.clone(),
        query: opened.query.clone(),
    };
    let id = &opened.running.manifest.plugin.id;
    match state
        .plugins
        .host()
        .render_as(
            &opened.running.plugin,
            request,
            Some(opened.viewer.clone()),
            &Default::default(),
        )
        .await
    {
        Ok(rendered) => {
            record_logs(&state.db, id, &source(&opened.path), &rendered.logs).await;
            Ok(rendered.page)
        }
        Err(err) => Err(render_error(state, opened, err).await),
    }
}

fn draw(opened: Opened, page: &Page, status: StatusCode, error: Option<String>) -> Response {
    let id = opened.running.manifest.plugin.id.clone();
    let tab = opened.tab.min(page.tabs.len().saturating_sub(1));
    // Tab links keep the page's query, and set only the host's `_tab`.
    let base_query: Vec<String> = opened
        .query
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    let tab_href = |i: usize| {
        let mut parts = base_query.clone();
        parts.push(format!("{TAB}={i}"));
        format!("{}?{}", page_href(&id, &opened.path), parts.join("&"))
    };
    let sections: Vec<SectionView> = page
        .sections
        .iter()
        .map(|s| section(&id, &opened.href, s))
        .collect();
    let tab_sections: Vec<SectionView> = page
        .tabs
        .get(tab)
        .map(|chosen| {
            chosen
                .sections
                .iter()
                .map(|s| section(&id, &opened.href, s))
                .collect()
        })
        .unwrap_or_default();
    let tabs = page
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| TabLink {
            label: t.label.clone(),
            href: tab_href(i),
            current: i == tab,
        })
        .collect();
    let watermark = format!(
        "Viewing as {} · {} EVE",
        opened.shell.user.name,
        chrono::Utc::now().format("%Y-%m-%d %H:%M")
    );
    render(
        status,
        &PluginPage {
            plugin_name: opened.running.manifest.plugin.name.clone(),
            shell: opened.shell,
            title: page.title.clone(),
            description: page.description.clone(),
            sections,
            tabs,
            tab_sections,
            error,
            watermark,
        },
    )
}

#[derive(Template)]
#[template(path = "dashboard_widget.html")]
struct WidgetFragment {
    title: String,
    /// The widget's page.
    href: String,
    sections: Vec<SectionView>,
    /// The plugin failed, or the viewer opened too many of its pages; its
    /// log says why (admins read it there).
    failed: bool,
    /// As on the page itself.
    watermark: String,
}

/// `GET /dashboard/widgets/{plugin}/{index}`: a plugin's Dashboard widget,
/// a fragment loaded after the Dashboard: its page's sections (not its
/// tabs). Checked and rate limited as opening the page is, so anyone who
/// may not open it gets the same 404 as for nothing. A plugin that fails
/// costs only its own widget.
pub async fn widget(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, index)): Path<(String, String)>,
) -> Result<Response, PageError> {
    // Signed in first: whether a plugin is installed is nobody else's
    // business.
    let session = session.ok_or_else(AppError::unauthorized)?;
    let index: usize = index.parse().map_err(|_| missing())?;
    manifest::check_id(&id).map_err(|_| missing())?;
    let running = state.plugins.running(&id).ok_or_else(missing)?;
    let widget = running
        .manifest
        .widgets
        .get(index)
        .cloned()
        .ok_or_else(missing)?;
    let href = page_href(&id, &widget.path);
    let watermark = |name: &str| {
        format!(
            "Viewing as {name} · {} EVE",
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        )
    };
    let unavailable = |title: String, href: String, watermark: String| WidgetFragment {
        title,
        href,
        sections: Vec::new(),
        failed: true,
        watermark,
    };
    let fragment = match open(&state, Some(session), &id, &widget.path, None).await {
        Ok((session, opened)) => {
            let mark = watermark(&opened.shell.user.name);
            // The page's own budget: a widget is a page view.
            if state
                .limits
                .plugin_pages
                .check((session.account.0, id.clone()), std::time::Instant::now())
                .is_err()
            {
                unavailable(widget.title, href, mark)
            } else {
                match render_page(&state, &opened).await {
                    Ok(page) => WidgetFragment {
                        title: widget.title,
                        sections: page
                            .sections
                            .iter()
                            .map(|s| section(&id, &opened.href, s))
                            .collect(),
                        href,
                        failed: false,
                        watermark: mark,
                    },
                    Err(_) => unavailable(widget.title, href, mark),
                }
            }
        }
        Err(err) if err.0.status() == StatusCode::NOT_FOUND => return Err(err),
        Err(err) if err.0.status() == StatusCode::UNAUTHORIZED => return Err(err),
        // No main yet, and the like: nothing to show, politely.
        Err(_) => unavailable(widget.title, href, String::new()),
    };
    Ok(render(StatusCode::OK, &fragment))
}

/// Percent-encodes a query component.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---- handlers ---------------------------------------------------------------

async fn show(
    state: AppState,
    session: Option<CurrentSession>,
    id: String,
    path: String,
    raw: Option<String>,
) -> Result<Response, PageError> {
    let (session, opened) = open(&state, session, &id, &path, raw.as_deref()).await?;
    if let Err(retry) = state
        .limits
        .plugin_pages
        .check((session.account.0, id), std::time::Instant::now())
    {
        return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
    }
    let page = render_page(&state, &opened).await?;
    Ok(draw(opened, &page, StatusCode::OK, None))
}

/// `GET /plugins/{id}`
pub async fn main_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, PageError> {
    show(state, session, id, String::new(), raw).await
}

/// `GET /plugins/{id}/{*path}`
pub async fn sub_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, path)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, PageError> {
    show(state, session, id, path, raw).await
}

async fn post(
    state: AppState,
    session: Option<CurrentSession>,
    id: String,
    path: String,
    raw: Option<String>,
    posted: Vec<(String, String)>,
) -> Result<Response, PageError> {
    let (session, opened) = open(&state, session, &id, &path, raw.as_deref()).await?;
    if let Err(retry) = state
        .limits
        .plugin_submits
        .check((session.account.0, id.clone()), std::time::Instant::now())
    {
        return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
    }
    if posted.len() > MAX_FORM_PAIRS {
        return Err(AppError::bad_request("That form has too many fields.").into());
    }
    let form_id = posted
        .iter()
        .find(|(k, _)| k == FORM)
        .map(|(_, v)| v.clone())
        .ok_or_else(|| AppError::bad_request("Send the form from its page."))?;
    let values: Vec<(String, String)> = posted.into_iter().filter(|(k, _)| k != FORM).collect();
    // The form as the plugin draws it now is what the values must fit.
    let page = render_page(&state, &opened).await?;
    let Some(form) = page_rules::find_form(&page, &form_id) else {
        return Ok(draw(
            opened,
            &page,
            StatusCode::CONFLICT,
            Some("That form isn't on this page any more. Try again.".to_owned()),
        ));
    };
    let values = match page_rules::check_submission(form, &values) {
        Ok(values) => values,
        Err(problem) => {
            return Ok(draw(
                opened,
                &page,
                StatusCode::UNPROCESSABLE_ENTITY,
                Some(problem),
            ));
        }
    };
    let submission = Submission {
        request: Request {
            path: opened.path.clone(),
            query: opened.query.clone(),
        },
        form: form_id,
        values,
    };
    let submitted = state
        .plugins
        .host()
        .submit_as(
            &opened.running.plugin,
            submission,
            Some(opened.viewer.clone()),
            &Default::default(),
        )
        .await;
    match submitted {
        Ok(submitted) => {
            record_logs(&state.db, &id, &source(&opened.path), &submitted.logs).await;
            match submitted.result {
                SubmitResult::Page(page) => Ok(draw(opened, &page, StatusCode::OK, None)),
                SubmitResult::Redirect(to) => {
                    Ok(Redirect::to(&page_href(&id, &to)).into_response())
                }
            }
        }
        Err(err) => Err(render_error(&state, &opened, err).await),
    }
}

/// `POST /plugins/{id}`: a form on the main page.
pub async fn post_main(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    Form(posted): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    post(state, session, id, String::new(), raw, posted).await
}

/// `POST /plugins/{id}/{*path}`
pub async fn post_sub(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, path)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    Form(posted): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    post(state, session, id, path, raw, posted).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isk_and_numbers_read_well() {
        assert_eq!(short_isk(1_240_000_000.0), "1.24b");
        assert_eq!(short_isk(350_200_000.0), "350.2m");
        assert_eq!(short_isk(12_500.0), "12.5k");
        assert_eq!(short_isk(999.0), "999");
        assert_eq!(short_isk(-2_000_000.0), "-2m");
        assert_eq!(grouped(1_240_000_000), "1,240,000,000");
        assert_eq!(grouped(-1234), "-1,234");
        assert_eq!(grouped(12), "12");
    }
}
