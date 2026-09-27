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
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use tether_plugins::host::{
    Action, Entity, EntityKind, FieldKind, Page, PageError as PluginPageError, Profile, Progress,
    RenderError, Request, Section, Submission, SubmitResult, Tone, Value,
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
    /// A link drawn as a primary button.
    pub primary: bool,
    /// A badge, and its Basecoat variant ("" for the default).
    pub badge: Option<&'static str>,
    /// Buttons that post; empty for other values.
    pub actions: Vec<ActionView>,
    /// Numbers, ISK and times: Geist Mono.
    pub mono: bool,
    pub entity: Option<EntityView>,
    pub countdown: Option<CountdownView>,
    pub progress: Option<ProgressView>,
    /// The host's Add owner button: its words, for a viewer who may add
    /// owners.
    pub add_owner: Option<AddOwnerView>,
    /// A link to share: one of the plugin's pages as its full address,
    /// which the host builds from the site's origin.
    pub share: Option<String>,
}

/// Tether's own Add owner form, posting to the host: the login comes back
/// to `back`, the page it's on.
pub struct AddOwnerView {
    pub label: String,
    pub plugin: String,
    pub back: String,
}

/// A character, corporation, alliance, faction or type: its picture from
/// CCP's image server (an address the host builds from the kind and id,
/// never one a plugin gave), or initials.
pub struct EntityView {
    pub name: String,
    pub image: Option<String>,
    pub initials: String,
    /// `sm` (20px) or `lg` (64px, a profile's subject).
    pub size: &'static str,
}

pub struct CountdownView {
    /// The instant in UTC, as the host wrote it (`2026-09-24T18:00:00Z`),
    /// for the script that ticks it.
    pub at: String,
    /// The time left when the page was drawn.
    pub text: String,
    /// The EVE time, on hover.
    pub title: String,
}

pub struct ProgressView {
    /// 0 to 1, with four decimals.
    pub value: String,
    pub percent: u32,
    pub label: Option<String>,
    /// Both or neither, in UTC as the host wrote them: the bar fills live
    /// between them.
    pub from: Option<String>,
    pub to: Option<String>,
}

/// A button that posts like a one-button form.
pub struct ActionView {
    pub label: String,
    /// The page's own address: it posts where the page's forms do.
    pub href: String,
    pub form: String,
    pub fields: Vec<(String, String)>,
    /// `outline`, `primary` or `danger`.
    pub style: &'static str,
    pub confirm: Option<String>,
    /// The confirmation's popover id, with `confirm`.
    pub popover: Option<String>,
}

pub struct BadgeView {
    pub label: String,
    pub variant: &'static str,
}

pub struct ProfileView {
    pub subject: EntityView,
    pub subtitle: Option<String>,
    pub corporation: Option<EntityView>,
    pub alliance: Option<EntityView>,
    pub facts: Vec<(String, ValueView)>,
    pub badges: Vec<BadgeView>,
}

/// A card of a grid: a profile drawn compactly, opening `href`.
pub struct CardItemView {
    pub profile: ProfileView,
    pub href: Option<String>,
}

pub struct CardsView {
    /// Tether's Register Character card first, for an app with user
    /// scopes that asks for it.
    pub register: bool,
    pub items: Vec<CardItemView>,
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
    Profile(ProfileView),
    Code(CodeView),
    Cards(CardsView),
}

pub struct CodeView {
    pub title: Option<String>,
    pub text: String,
    pub copy_label: String,
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

/// A badge's Basecoat variant ("" for the default, the accent).
fn badge_variant(tone: &Tone) -> &'static str {
    match tone {
        Tone::Neutral | Tone::Warning => "outline",
        Tone::Success => "secondary",
        Tone::Danger => "destructive",
        Tone::Accent => "",
    }
}

/// CCP's image for an entity: built here from its kind and id only, so a
/// plugin can't point a browser anywhere. `None` without an id (fixtures,
/// or a plugin that doesn't know it): initials instead.
pub fn entity_image(kind: &EntityKind, id: i64, size: u32) -> Option<String> {
    if id <= 0 {
        return None;
    }
    Some(match kind {
        EntityKind::Character => {
            format!("https://images.evetech.net/characters/{id}/portrait?size={size}")
        }
        EntityKind::Corporation => {
            format!("https://images.evetech.net/corporations/{id}/logo?size={size}")
        }
        EntityKind::Alliance => {
            format!("https://images.evetech.net/alliances/{id}/logo?size={size}")
        }
        // Factions' logos are served as corporations'.
        EntityKind::Faction => {
            format!("https://images.evetech.net/corporations/{id}/logo?size={size}")
        }
        EntityKind::Type => format!("https://images.evetech.net/types/{id}/icon?size={size}"),
    })
}

/// An entity at 20px (64px pictures, for sharp screens; types' 32px
/// icons), or at 64px as a profile's subject (128px pictures; types' 64px
/// icons, their largest).
fn entity(entity: &Entity, large: bool) -> EntityView {
    let is_type = matches!(entity.kind, EntityKind::Type);
    let (size, pixels) = match (large, is_type) {
        (false, false) => ("sm", 64),
        (false, true) => ("sm", 32),
        (true, false) => ("lg", 128),
        (true, true) => ("lg", 64),
    };
    EntityView {
        name: entity.name.clone(),
        image: entity_image(&entity.kind, entity.id, pixels),
        initials: super::initials(&entity.name),
        size,
    }
}

/// The time left until an instant: `2d 4h 13m`, `4h 13m`, `13m 05s`,
/// `45s`; `done` once it has passed. `assets/live.js` writes the same.
pub fn countdown_text(seconds_left: i64) -> String {
    if seconds_left <= 0 {
        return "done".to_owned();
    }
    let (d, h, m, s) = (
        seconds_left / 86_400,
        seconds_left % 86_400 / 3_600,
        seconds_left % 3_600 / 60,
        seconds_left % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// An RFC 3339 instant (checked by the host already) in UTC.
fn utc(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&chrono::Utc))
}

/// The one way the host writes an instant into a page: UTC, whole
/// seconds, `Z`. Never the plugin's own text.
fn machine_time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn progress(progress: &Progress, now: chrono::DateTime<chrono::Utc>) -> ProgressView {
    let span = progress
        .from
        .as_deref()
        .and_then(utc)
        .zip(progress.to.as_deref().and_then(utc))
        .filter(|(from, to)| to > from);
    let fraction = match span {
        Some((from, to)) => {
            let done = (now - from).num_milliseconds() as f64;
            let whole = (to - from).num_milliseconds() as f64;
            (done / whole).clamp(0.0, 1.0)
        }
        None => progress.fraction.clamp(0.0, 1.0),
    };
    ProgressView {
        value: format!("{fraction:.4}"),
        percent: (fraction * 100.0).floor() as u32,
        label: progress.label.clone(),
        from: span.map(|(from, _)| machine_time(from)),
        to: span.map(|(_, to)| machine_time(to)),
    }
}

/// What drawing a page's sections needs: whose page it is, where its forms
/// and actions post, and ids for confirmation popovers (unique on the
/// screen: a Dashboard shows several widgets).
pub struct Ctx<'a> {
    pub plugin: &'a str,
    /// The page's address with its query: forms and actions post here.
    pub action: &'a str,
    /// Starts every popover id drawn with this context.
    pub prefix: String,
    /// Whether the app reads members' characters (user scopes), so its
    /// card grids may start with Tether's Register Character card.
    pub registers: bool,
    /// The site's origin (as the direct join link's): links to share start
    /// with it.
    pub site: &'a str,
    /// For a viewer who may add the app's owners: the page's link path,
    /// which Add owner's login comes back to. `None` draws no Add owner.
    pub owner_back: Option<String>,
    next: std::cell::Cell<usize>,
}

impl<'a> Ctx<'a> {
    pub fn new(plugin: &'a str, action: &'a str, prefix: String, site: &'a str) -> Self {
        Self {
            plugin,
            action,
            prefix,
            registers: false,
            site,
            owner_back: None,
            next: std::cell::Cell::new(0),
        }
    }

    /// For a viewer who may add owners, on the page at `back` (see
    /// `owner_back`).
    pub fn adding_owners(mut self, back: Option<String>) -> Self {
        self.owner_back = back;
        self
    }

    /// For an app with user scopes (see `registers`).
    pub fn registering(mut self, registers: bool) -> Self {
        self.registers = registers;
        self
    }

    fn next_id(&self) -> String {
        let n = self.next.get();
        self.next.set(n + 1);
        format!("{}-confirm-{n}", self.prefix)
    }
}

fn action_view(ctx: &Ctx, action: &Action) -> ActionView {
    ActionView {
        label: action.label.clone(),
        href: ctx.action.to_owned(),
        form: action.form.clone(),
        fields: action.fields.clone(),
        style: match action.tone {
            Tone::Danger => "danger",
            Tone::Accent => "primary",
            Tone::Neutral | Tone::Success | Tone::Warning => "outline",
        },
        confirm: action.confirm.clone(),
        popover: action.confirm.as_ref().map(|_| ctx.next_id()),
    }
}

fn value(ctx: &Ctx, value: &Value) -> ValueView {
    let plugin = ctx.plugin;
    let plain = |text: String| ValueView {
        text,
        title: None,
        href: None,
        primary: false,
        badge: None,
        mono: false,
        entity: None,
        countdown: None,
        progress: None,
        actions: Vec::new(),
        share: None,
        add_owner: None,
    };
    let mono = |text: String| ValueView {
        mono: true,
        ..plain(text)
    };
    match value {
        Value::Text(text) => plain(text.clone()),
        Value::Number(n) => mono(grouped(*n)),
        Value::Isk(amount) => ValueView {
            title: Some(format!("{} ISK", grouped(amount.trunc() as i64))),
            ..mono(short_isk(*amount))
        },
        Value::Time(text) => {
            // Checked as RFC 3339 by the host already.
            let shown = utc(text)
                .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| text.clone());
            ValueView {
                title: Some(format!("{text} (EVE time)")),
                ..mono(shown)
            }
        }
        Value::Badge(badge) => ValueView {
            badge: Some(badge_variant(&badge.tone)),
            ..plain(badge.label.clone())
        },
        Value::Link(link) => ValueView {
            href: Some(page_href(plugin, &link.path)),
            primary: link.primary,
            ..plain(link.label.clone())
        },
        Value::Action(action) => ValueView {
            actions: vec![action_view(ctx, action)],
            ..plain(String::new())
        },
        Value::Actions(actions) => ValueView {
            actions: actions.iter().map(|a| action_view(ctx, a)).collect(),
            ..plain(String::new())
        },
        Value::Entity(e) => ValueView {
            entity: Some(entity(e, false)),
            ..plain(e.name.clone())
        },
        // A checked link path, under the plugin's own pages: the address
        // is the site's and the plugin's, never one the plugin wrote.
        Value::Share(path) => ValueView {
            share: Some(format!("{}{}", ctx.site, page_href(plugin, path))),
            ..plain(String::new())
        },
        Value::AddOwner(label) => ValueView {
            add_owner: ctx.owner_back.as_ref().map(|back| AddOwnerView {
                label: if label.trim().is_empty() {
                    "Add owner".to_owned()
                } else {
                    label.clone()
                },
                plugin: plugin.to_owned(),
                back: back.clone(),
            }),
            ..plain(String::new())
        },
        Value::Countdown(text) => match utc(text) {
            Some(at) => {
                let left = (at - chrono::Utc::now()).num_seconds();
                let shown = countdown_text(left);
                ValueView {
                    countdown: Some(CountdownView {
                        at: machine_time(at),
                        text: shown.clone(),
                        title: format!("{} EVE", at.format("%Y-%m-%d %H:%M:%S")),
                    }),
                    ..mono(shown)
                }
            }
            None => plain(text.clone()),
        },
        Value::Progress(p) => {
            let view = progress(p, chrono::Utc::now());
            ValueView {
                progress: Some(view),
                ..plain(String::new())
            }
        }
    }
}

fn profile(ctx: &Ctx, p: &Profile) -> ProfileView {
    ProfileView {
        subject: entity(&p.subject, true),
        subtitle: p.subtitle.clone(),
        corporation: p.corporation.as_ref().map(|e| entity(e, false)),
        alliance: p.alliance.as_ref().map(|e| entity(e, false)),
        facts: p
            .facts
            .iter()
            .map(|(label, v)| (label.clone(), value(ctx, v)))
            .collect(),
        badges: p
            .badges
            .iter()
            .map(|b| BadgeView {
                label: b.label.clone(),
                variant: badge_variant(&b.tone),
            })
            .collect(),
    }
}

fn number_text(n: Option<f64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_default()
}

fn section(ctx: &Ctx, section: &Section) -> SectionView {
    match section {
        Section::Stats(stats) => SectionView::Stats(
            stats
                .iter()
                .map(|s| StatView {
                    label: s.label.clone(),
                    value: value(ctx, &s.value),
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
                .map(|row| row.iter().map(|v| value(ctx, v)).collect())
                .collect(),
            empty: table.empty.clone(),
        }),
        Section::Card(card) => SectionView::Card(CardView {
            title: card.title.clone(),
            description: card.description.clone(),
            fields: card
                .fields
                .iter()
                .map(|(label, v)| (label.clone(), value(ctx, v)))
                .collect(),
        }),
        Section::Text(text) => SectionView::Text(text.clone()),
        Section::Profile(p) => SectionView::Profile(profile(ctx, p)),
        Section::Cards(grid) => SectionView::Cards(CardsView {
            register: grid.register && ctx.registers,
            items: grid
                .items
                .iter()
                .map(|card| CardItemView {
                    profile: profile(ctx, &card.profile),
                    href: card.link.as_deref().map(|path| page_href(ctx.plugin, path)),
                })
                .collect(),
        }),
        Section::Code(code) => SectionView::Code(CodeView {
            title: code.title.clone(),
            text: code.text.clone(),
            copy_label: code.copy_label.clone().unwrap_or_else(|| "Copy".to_owned()),
        }),
        Section::Form(form) => SectionView::Form(FormView {
            id: form.id.clone(),
            action: ctx.action.to_owned(),
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

/// Everything below the top bar: what a live page reloads.
pub struct ContentView {
    pub title: String,
    pub description: Option<String>,
    /// The page's own links beside the title.
    pub links: Vec<TabLink>,
    /// Its primary links, drawn as buttons after them.
    pub buttons: Vec<TabLink>,
    pub sections: Vec<SectionView>,
    pub tabs: Vec<TabLink>,
    pub tab_sections: Vec<SectionView>,
    pub error: Option<String>,
    pub watermark: String,
    /// Seconds between reloads, while the page asks for them.
    pub refresh: Option<u32>,
    /// The page's address with its query: what a reload fetches.
    pub href: String,
    /// The app's owners (Add owner), drawn by the host.
    pub owners: Option<super::plugin_access::Owners>,
}

#[derive(Template)]
#[template(path = "plugin_page.html")]
struct PluginPage {
    shell: Shell,
    plugin_name: String,
    c: ContentView,
}

/// A live page's content, reloaded in place (`plugin_content.html`).
#[derive(Template)]
#[template(path = "plugin_content.html")]
struct PluginContent {
    c: ContentView,
}

/// The id of a live page's content: htmx names it in `HX-Trigger` when it
/// reloads it.
const CONTENT_ID: &str = "plugin-content";

// ---- access -----------------------------------------------------------------

/// What a request for a plugin page has been checked into.
struct Opened {
    shell: Shell,
    running: Running,
    /// The signed-in account (the audit log's actor).
    account: tether_db::accounts::AccountId,
    /// Who is looking, for `identity.current`.
    viewer: Viewer,
    path: String,
    query: Vec<(String, String)>,
    tab: usize,
    /// The page's own address, with its query: forms post back here.
    href: String,
    owners: Option<super::plugin_access::Owners>,
    /// The site's origin, for links to share.
    site: String,
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
    let owners =
        super::plugin_access::owners(state, &session, &running, &perms, path.is_empty()).await?;
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
    let account = session.account;
    Ok((
        session,
        Opened {
            shell,
            running,
            account,
            viewer,
            path: path.to_owned(),
            query,
            tab,
            href,
            owners,
            site: state.site.origin().to_owned(),
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
        // Only names this plugin declares: ids can nest (acme.esi and
        // acme.esi.extra), so a prefix alone could leak another plugin's.
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

/// How a page came to be rendered, for the audit log.
#[derive(Clone, Copy)]
enum Via {
    Page,
    Reload,
    Form,
    Widget,
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Page => "page",
            Via::Reload => "reload",
            Via::Form => "form",
            Via::Widget => "widget",
        }
    }
}

/// Renders the page for the viewer. A page under an audited `[[pages]]`
/// rule is written to the audit log first (`plugin.page_view`), and isn't
/// shown if that fails.
async fn render_page(state: &AppState, opened: &Opened, via: Via) -> Result<Page, PageError> {
    let id = &opened.running.manifest.plugin.id;
    if opened.running.manifest.page_audited(&opened.path) {
        tether_db::audit::record(
            &state.db,
            tether_db::audit::Actor::Account(opened.account),
            "plugin.page_view",
            Some(&format!("plugin:{id}")),
            serde_json::json!({
                "path": opened.path,
                "query": opened.query,
                "via": via.as_str(),
            }),
        )
        .await
        .map_err(AppError::from)?;
    }
    let request = Request {
        path: opened.path.clone(),
        query: opened.query.clone(),
    };
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

/// The page's path, for Add owner to come back to, if the viewer may add
/// the app's owners (a browser session holding the app's add permission).
fn owner_back(opened: &Opened) -> Option<String> {
    opened
        .owners
        .as_ref()
        .filter(|o| o.can_offer)
        .map(|_| opened.path.clone())
}

/// Draws a page, or with `reload` only its content, for a live page
/// reloading itself.
fn draw(
    opened: Opened,
    page: &Page,
    status: StatusCode,
    error: Option<String>,
    reload: bool,
) -> Response {
    let id = opened.running.manifest.plugin.id.clone();
    let audited = opened.running.manifest.page_audited(&opened.path);
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
    let ctx = Ctx::new(&id, &opened.href, "page".to_owned(), &opened.site)
        .registering(!opened.running.manifest.capabilities.esi.user.is_empty())
        .adding_owners(owner_back(&opened));
    let sections: Vec<SectionView> = page.sections.iter().map(|s| section(&ctx, s)).collect();
    let tab_sections: Vec<SectionView> = page
        .tabs
        .get(tab)
        .map(|chosen| chosen.sections.iter().map(|s| section(&ctx, s)).collect())
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
    let header_link = |l: &tether_plugins::host::Link| TabLink {
        label: l.label.clone(),
        href: page_href(&id, &l.path),
        current: l.path == opened.path,
    };
    let links = page
        .links
        .iter()
        .filter(|l| !l.primary)
        .map(header_link)
        .collect();
    let buttons = page
        .links
        .iter()
        .filter(|l| l.primary)
        .map(header_link)
        .collect();
    let watermark = format!(
        "Viewing as {} · {} EVE",
        opened.shell.user.name,
        chrono::Utc::now().format("%Y-%m-%d %H:%M")
    );
    let content = ContentView {
        title: page.title.clone(),
        description: page.description.clone(),
        links,
        buttons,
        sections,
        tabs,
        tab_sections,
        // A page showing a problem with a post isn't reloaded: that would
        // take the problem away.
        // Nor is an audited page: every reload would be an audit entry, and
        // an open tab would bury the log.
        refresh: if error.is_none() && !audited {
            page_rules::refresh_seconds(page)
        } else {
            None
        },
        error,
        watermark,
        href: opened.href.clone(),
        owners: opened.owners,
    };
    let mut response = if reload {
        render(status, &PluginContent { c: content })
    } else {
        render(
            status,
            &PluginPage {
                plugin_name: opened.running.manifest.plugin.name.clone(),
                shell: opened.shell,
                c: content,
            },
        )
    };
    let headers = response.headers_mut();
    // The page and its content alone share an address: caches must tell
    // them apart. Content alone, and audited pages, aren't kept at all (a
    // page shown again from history would be an unrecorded view).
    headers.insert(
        header::VARY,
        HeaderValue::from_static("HX-Request, HX-Trigger"),
    );
    if reload || audited {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
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
                match render_page(&state, &opened, Via::Widget).await {
                    Ok(page) => WidgetFragment {
                        title: widget.title,
                        sections: {
                            // Popover ids unique among the Dashboard's
                            // widgets.
                            let ctx = Ctx::new(
                                &id,
                                &opened.href,
                                format!("widget-{index}-{id}"),
                                &opened.site,
                            )
                            .registering(!opened.running.manifest.capabilities.esi.user.is_empty())
                            .adding_owners(owner_back(&opened));
                            page.sections.iter().map(|s| section(&ctx, s)).collect()
                        },
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
pub(crate) fn encode(text: &str) -> String {
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

/// Whether a request is a live page reloading its content: htmx names the
/// element that asked in `HX-Trigger`.
fn is_reload(headers: &HeaderMap) -> bool {
    super::is_htmx(headers)
        && headers
            .get("hx-trigger")
            .is_some_and(|v| v.as_bytes() == CONTENT_ID.as_bytes())
}

async fn show(
    state: AppState,
    session: Option<CurrentSession>,
    id: String,
    path: String,
    raw: Option<String>,
    headers: HeaderMap,
) -> Result<Response, PageError> {
    let reload = is_reload(&headers);
    let shown = async {
        let (session, opened) = open(&state, session, &id, &path, raw.as_deref()).await?;
        if let Err(retry) = state
            .limits
            .plugin_pages
            .check((session.account.0, id.clone()), std::time::Instant::now())
        {
            return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
        }
        let via = if reload { Via::Reload } else { Via::Page };
        let page = render_page(&state, &opened, via).await?;
        Ok::<_, PageError>(draw(opened, &page, StatusCode::OK, None, reload))
    }
    .await;
    match shown {
        Err(err) if reload => Ok(reload_failed(err.0.status())),
        other => other,
    }
}

/// A reload that didn't work leaves the content as it is (204), to try
/// again at the next interval; or, if the page is gone for this viewer
/// (signed out, no longer allowed, uninstalled), reloads the whole page,
/// which then says so.
fn reload_failed(status: StatusCode) -> Response {
    let gone = matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    );
    if gone {
        (StatusCode::NO_CONTENT, [("hx-refresh", "true")]).into_response()
    } else {
        StatusCode::NO_CONTENT.into_response()
    }
}

/// `GET /plugins/{id}`
pub async fn main_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, PageError> {
    show(state, session, id, String::new(), raw, headers).await
}

/// `GET /plugins/{id}/{*path}`
pub async fn sub_page(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, path)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Result<Response, PageError> {
    show(state, session, id, path, raw, headers).await
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
    // So is an action button: the page must offer this very one (its form
    // and hidden values) to this person now, and the plugin gets the
    // values as the page drew them.
    let page = render_page(&state, &opened, Via::Form).await?;
    let values = if let Some(form) = page_rules::find_form(&page, &form_id) {
        match page_rules::check_submission(form, &values) {
            Ok(values) => values,
            Err(problem) => {
                return Ok(draw(
                    opened,
                    &page,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Some(problem),
                    false,
                ));
            }
        }
    } else if let Some(action) = page_rules::find_action(&page, &form_id, &values) {
        action.fields.clone()
    } else {
        return Ok(draw(
            opened,
            &page,
            StatusCode::CONFLICT,
            Some("That form isn't on this page any more. Try again.".to_owned()),
            false,
        ));
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
                SubmitResult::Page(page) => Ok(draw(opened, &page, StatusCode::OK, None, false)),
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

    #[test]
    fn countdowns_read_as_live_js_writes_them() {
        assert_eq!(
            countdown_text(2 * 86_400 + 4 * 3_600 + 13 * 60 + 9),
            "2d 4h 13m"
        );
        assert_eq!(countdown_text(4 * 3_600 + 13 * 60), "4h 13m");
        assert_eq!(countdown_text(13 * 60 + 5), "13m 05s");
        assert_eq!(countdown_text(45), "45s");
        assert_eq!(countdown_text(0), "done");
        assert_eq!(countdown_text(-3_600), "done");
    }

    #[test]
    fn images_come_only_from_kind_and_id() {
        assert_eq!(
            entity_image(&EntityKind::Type, 587, 32).as_deref(),
            Some("https://images.evetech.net/types/587/icon?size=32")
        );
        assert_eq!(
            entity_image(&EntityKind::Alliance, 99, 64).as_deref(),
            Some("https://images.evetech.net/alliances/99/logo?size=64")
        );
        assert_eq!(entity_image(&EntityKind::Character, 0, 64), None);
        assert_eq!(entity_image(&EntityKind::Character, -1, 64), None);
    }

    #[test]
    fn progress_between_instants_follows_the_clock() {
        let at = |t: &str| utc(t).unwrap();
        let bar = Progress {
            fraction: 0.9,
            from: Some("2026-09-24T18:00:00+02:00".to_owned()),
            to: Some("2026-09-24T20:00:00+02:00".to_owned()),
            label: None,
        };
        let view = progress(&bar, at("2026-09-24T16:30:00Z"));
        assert_eq!(view.value, "0.2500");
        assert_eq!(view.percent, 25);
        assert_eq!(view.from.as_deref(), Some("2026-09-24T16:00:00Z"));
        assert_eq!(progress(&bar, at("2027-01-01T00:00:00Z")).percent, 100);
        assert_eq!(progress(&bar, at("2020-01-01T00:00:00Z")).percent, 0);
        let fixed = Progress {
            from: None,
            to: None,
            ..bar
        };
        assert_eq!(progress(&fixed, at("2020-01-01T00:00:00Z")).percent, 90);
    }

    #[test]
    fn only_apps_with_user_scopes_draw_the_register_card() {
        let grid = Section::Cards(tether_plugins::host::CardGrid {
            items: Vec::new(),
            register: true,
        });
        let drawn = |registers: bool| {
            let ctx = Ctx::new(
                "acme.x",
                "/plugins/acme.x",
                "page".to_owned(),
                "https://a.example",
            )
            .registering(registers);
            match section(&ctx, &grid) {
                SectionView::Cards(cards) => cards.register,
                _ => panic!("not a card grid"),
            }
        };
        assert!(drawn(true));
        assert!(!drawn(false));
    }

    #[test]
    fn links_to_share_are_the_sites_own_address() {
        let ctx = Ctx::new(
            "acme.fat",
            "/plugins/acme.fat",
            "page".to_owned(),
            "https://auth.example.com",
        );
        let view = value(&ctx, &Value::Share("links/0f3a/add".to_owned()));
        assert_eq!(
            view.share.as_deref(),
            Some("https://auth.example.com/plugins/acme.fat/links/0f3a/add")
        );
        assert!(view.href.is_none());
    }

    #[test]
    fn add_owner_is_drawn_only_for_those_who_may_add_owners() {
        let ctx = |back: Option<&str>| {
            Ctx::new(
                "acme.fat",
                "/plugins/acme.fat/links/create",
                "page".to_owned(),
                "",
            )
            .adding_owners(back.map(str::to_owned))
        };
        let label = Value::AddOwner("Log in with the fleet boss".to_owned());
        assert!(value(&ctx(None), &label).add_owner.is_none());
        let drawn = value(&ctx(Some("links/create")), &label)
            .add_owner
            .expect("drawn");
        assert_eq!(drawn.label, "Log in with the fleet boss");
        assert_eq!(drawn.plugin, "acme.fat");
        assert_eq!(drawn.back, "links/create");
        let unnamed = value(&ctx(Some("")), &Value::AddOwner(" ".to_owned()))
            .add_owner
            .expect("drawn");
        assert_eq!(unnamed.label, "Add owner");
    }
}
