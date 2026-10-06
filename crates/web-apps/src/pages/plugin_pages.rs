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

use super::stay::{Toast, with_toast};
use super::{CardFoot, PageError, Shell, encode, grouped, load, render};
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
/// The host's per-table page numbers: `_p0=2` is the page's first table on
/// its second page.
const TABLE_PAGE: &str = "_p";
/// Rows a table shows at once (Jay, 2026-09-30).
pub const ROWS_PER_PAGE: usize = 25;
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
    /// A link to one of the app's downloads: fetched as a file, not
    /// swapped in as a page.
    pub download: bool,
    /// A badge (a label: a neutral or highlighted chip), and its Basecoat
    /// variant ("" for the default).
    pub badge: Option<&'static str>,
    /// A status (a badge with a tone of success, warning or danger), drawn
    /// as a status line (DESIGN.md): a small square in the tone's colour
    /// and the word. "ok", "warn" or "danger".
    pub status: Option<&'static str>,
    /// Buttons that post; empty for other values.
    pub actions: Vec<ActionView>,
    /// Numbers, ISK and times: IBM Plex Mono.
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
    /// Instruments (DESIGN.md): skill levels, a composition ring, shield,
    /// armor and hull.
    pub levels: Option<super::plugin_visuals::LevelsView>,
    pub composition: Option<super::plugin_visuals::CompositionView>,
    pub defenses: Option<super::plugin_visuals::DefensesView>,
    /// A muted unit after the value (DESIGN.md, Data display): ISK, EVE.
    pub unit: Option<&'static str>,
    /// What a table sorts it by (`data-sort`, assets/live.js): seconds for
    /// times and countdowns, the plain number for counts and ISK.
    pub sort: Option<String>,
}

impl ValueView {
    /// Nothing to show: the host draws "—" for it where a value is
    /// expected (a labelled column, a fact).
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
            && self.href.is_none()
            && self.actions.is_empty()
            && self.entity.is_none()
            && self.countdown.is_none()
            && self.progress.is_none()
            && self.add_owner.is_none()
            && self.share.is_none()
            && self.levels.is_none()
            && self.composition.is_none()
            && self.defenses.is_none()
    }
}

/// Tether's own Add owner form, posting to the host: the login comes back
/// to `back`, the page it's on.
pub struct AddOwnerView {
    pub label: String,
    pub plugin: String,
    pub back: String,
    /// What EVE will ask for: the app's data-source scopes, in the host's
    /// own words beside the app's.
    pub scopes: Vec<String>,
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
    /// The segmented bar's cells (DESIGN.md): lit or not, [`SEGMENTS`] of
    /// them.
    pub cells: Vec<bool>,
}

/// Cells in a segmented bar.
pub const SEGMENTS: usize = 24;

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
    /// The popup its form is drawn in, for an action opening one of the
    /// page's forms; its hidden fields as JSON, which the popup's form
    /// posts with its own.
    pub opens: Option<String>,
    pub fields_json: String,
}

pub struct BadgeView {
    pub label: String,
    pub variant: &'static str,
    /// A status, drawn as a status line instead (see [`badge_kind`]).
    pub status: Option<&'static str>,
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
    /// Tether's footer, for one of the viewer's own characters on the
    /// Dashboard.
    pub foot: Option<CardFoot>,
}

pub struct CardsView {
    /// Tether's Register Character card, for an app with user scopes that
    /// asks for it: where it leads (registering for the app). A grid with
    /// it is the account's characters, drawn as a roster (DESIGN.md, Card
    /// grid), the card a row at its end.
    pub register: Option<String>,
    pub items: Vec<CardItemView>,
    /// The roster's header band: the facts' labels, from the first
    /// character that has any.
    pub columns: Vec<String>,
}

pub struct StatView {
    pub label: String,
    pub value: ValueView,
    pub caption: Option<String>,
    /// A number that needs someone ("warn", the signal) or is a problem
    /// ("danger"), from the app's badge: drawn as the number in that
    /// colour, never a badge in a stat card (DESIGN.md, Stat cards).
    pub tone: Option<&'static str>,
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
    /// Longer than a page: which one this is, and the way to the others.
    pub pager: Option<Pager>,
}

/// A long table's place: rows `from` to `to` of `total`.
pub struct Pager {
    pub from: usize,
    pub to: usize,
    pub total: usize,
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// Which table a `_pN` names, if it's one.
fn table_page_key(key: &str) -> Option<usize> {
    key.strip_prefix(TABLE_PAGE)?.parse().ok()
}

/// Pages every table longer than [`ROWS_PER_PAGE`], numbering the page's
/// tables in order (a row's too); `href(n, page)` is the address of table
/// `n` on `page`.
fn paginate(
    views: &mut [SectionView],
    next_index: &mut usize,
    pages: &[(usize, usize)],
    href: &dyn Fn(usize, usize) -> String,
) {
    for view in views {
        match view {
            SectionView::Row(members) => paginate(members, next_index, pages, href),
            SectionView::Table(table) => page_table(table, next_index, pages, href),
            // Merged tables: each its own pages, as it was before.
            SectionView::Tables(set) => {
                for table in &mut set.groups {
                    page_table(table, next_index, pages, href);
                }
            }
            _ => {}
        }
    }
}

/// One table's page, numbered `next_index` (and counted).
fn page_table(
    table: &mut TableView,
    next_index: &mut usize,
    pages: &[(usize, usize)],
    href: &dyn Fn(usize, usize) -> String,
) {
    let index = *next_index;
    *next_index += 1;
    let total = table.rows.len();
    if total <= ROWS_PER_PAGE {
        return;
    }
    let count = total.div_ceil(ROWS_PER_PAGE);
    let page = pages
        .iter()
        .find(|(n, _)| *n == index)
        .map_or(1, |(_, p)| *p)
        .clamp(1, count);
    let from = (page - 1) * ROWS_PER_PAGE;
    let to = (from + ROWS_PER_PAGE).min(total);
    table.rows = table.rows.drain(from..to).collect();
    table.pager = Some(Pager {
        from: from + 1,
        to,
        total,
        previous: (page > 1).then(|| href(index, page - 1)),
        next: (page < count).then(|| href(index, page + 1)),
    });
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
    /// An optional select gets Tether's empty choice ("—") unless the app
    /// named its own ("Any rarity", "Not posted").
    pub blank: bool,
}

pub struct FormView {
    pub id: String,
    /// Drawn in a popup (an action opens it): its id.
    pub popup: Option<String>,
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
    Timeline(super::plugin_visuals::TimelineView),
    /// Tables in a row with the same columns, drawn as one (see
    /// [`arrange`]).
    Tables(TablesView),
    /// Narrow sections side by side (see [`arrange`]).
    Row(Vec<SectionView>),
}

/// Tables that followed each other with the same columns: one table, each
/// one's title a heading row, so their columns line up. Every group's rows
/// have exactly `columns.len()` cells: `page::check` refuses any other row
/// before a page is drawn, and [`arrange`] only joins tables with as many
/// columns (the template indexes `columns` by cell). The groups' own
/// `columns` are left empty: read these.
pub struct TablesView {
    pub columns: Vec<ColumnView>,
    pub groups: Vec<TableView>,
}

impl SectionView {
    /// Fits half a row, and with what: code blocks pair with code blocks
    /// (EFT beside Buy All), tables of up to three columns with each other
    /// (fresh moons beside old ones).
    fn narrow(&self) -> Option<&'static str> {
        match self {
            SectionView::Code(_) => Some("code"),
            SectionView::Table(t) if t.columns.len() <= NARROW_COLUMNS => Some("table"),
            _ => None,
        }
    }

    /// Whether it sits beside `other` in a row: the same narrow kind, and
    /// for tables of a like length (a one-row table beside 237 rows reads
    /// as a stray scrap, not a pair).
    fn pairs_with(&self, other: &SectionView) -> bool {
        let rows = |v: &SectionView| match v {
            SectionView::Table(t) => t.rows.len(),
            _ => 0,
        };
        let (a, b) = (rows(self), rows(other));
        self.narrow().is_some() && self.narrow() == other.narrow() && a.max(b) <= 3 * a.min(b) + 5
    }
}

/// A table this narrow fits half a row; a wider one lines up with the
/// tables after it that share its columns.
const NARROW_COLUMNS: usize = 3;

/// Lays an app's sections out the way DESIGN.md's Plugins section says,
/// without the app asking: titled tables that follow each other with the
/// same columns (four or more) become one, their columns lined up; and
/// narrow sections of a kind that follow each other (code blocks, or
/// tables of up to three columns) go side by side, two to a row where
/// there's room. The order never changes.
pub fn arrange(views: Vec<SectionView>) -> Vec<SectionView> {
    let same = |a: &TableView, b: &[ColumnView]| {
        a.columns.len() == b.len()
            && a.columns
                .iter()
                .zip(b)
                .all(|(x, y)| x.label == y.label && x.numeric == y.numeric)
    };
    let mut merged: Vec<SectionView> = Vec::new();
    for view in views {
        let joins = match (&view, merged.last()) {
            (SectionView::Table(t), Some(SectionView::Table(last))) => {
                t.title.is_some()
                    && last.title.is_some()
                    && t.columns.len() > NARROW_COLUMNS
                    && same(t, &last.columns)
            }
            (SectionView::Table(t), Some(SectionView::Tables(set))) => {
                t.title.is_some() && same(t, &set.columns)
            }
            _ => false,
        };
        if !joins {
            merged.push(view);
            continue;
        }
        let SectionView::Table(mut table) = view else {
            continue;
        };
        match merged.pop() {
            Some(SectionView::Table(mut first)) => {
                let columns = std::mem::take(&mut first.columns);
                table.columns.clear();
                merged.push(SectionView::Tables(TablesView {
                    columns,
                    groups: vec![first, table],
                }));
            }
            Some(SectionView::Tables(mut set)) => {
                table.columns.clear();
                set.groups.push(table);
                merged.push(SectionView::Tables(set));
            }
            // `joins` holds only after a table or a set of them.
            Some(other) => merged.push(other),
            None => {}
        }
    }
    let mut out: Vec<SectionView> = Vec::new();
    let mut run: Vec<SectionView> = Vec::new();
    let flush = |run: &mut Vec<SectionView>, out: &mut Vec<SectionView>| match run.len() {
        0 => {}
        1 => out.append(run),
        _ => out.push(SectionView::Row(std::mem::take(run))),
    };
    for view in merged {
        match view.narrow() {
            Some(_) if run.last().is_some_and(|last| view.pairs_with(last)) => {
                run.push(view);
            }
            Some(_) => {
                flush(&mut run, &mut out);
                run.push(view);
            }
            None => {
                flush(&mut run, &mut out);
                out.push(view);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

pub struct CodeView {
    pub title: Option<String>,
    pub text: String,
    pub copy_label: String,
}

#[derive(Clone)]
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
    } else if abs < 0.5 {
        // Not "-0" for a tiny negative (or negative zero) amount.
        return "0".to_owned();
    } else {
        return format!("{amount:.0}");
    };
    let text = format!("{scaled:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}{unit}")
}

/// A badge's Basecoat variant ("" for the default, the accent).
/// How a badge is drawn (DESIGN.md, Status lines): one with a tone of
/// success, warning or danger says how something is, so it's a status
/// line, `Err` with its tone; a neutral or highlighted one is a label, a
/// chip, `Ok` with its Basecoat variant.
fn badge_kind(tone: &Tone) -> Result<&'static str, &'static str> {
    match tone {
        Tone::Neutral => Ok("outline"),
        Tone::Accent => Ok(""),
        Tone::Success => Err("ok"),
        Tone::Warning => Err("warn"),
        Tone::Danger => Err("danger"),
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
    // "T−" (a real minus sign) and two units, the second padded, as
    // DESIGN.md's countdowns: T− 2d 04h, T− 19h 40m, T− 13m 05s.
    if d > 0 {
        format!("T\u{2212} {d}d {h:02}h")
    } else if h > 0 {
        format!("T\u{2212} {h}h {m:02}m")
    } else if m > 0 {
        format!("T\u{2212} {m}m {s:02}s")
    } else {
        format!("T\u{2212} {s}s")
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
    let lit = (fraction * SEGMENTS as f64).round() as usize;
    ProgressView {
        value: format!("{fraction:.4}"),
        percent: (fraction * 100.0).floor() as u32,
        label: progress.label.clone(),
        from: span.map(|(from, _)| machine_time(from)),
        to: span.map(|(_, to)| machine_time(to)),
        cells: (0..SEGMENTS).map(|i| i < lit).collect(),
    }
}

/// What drawing a page's sections needs: whose page it is, where its forms
/// and actions post, and ids for confirmation popovers (unique on the
/// screen).
pub struct Ctx<'a> {
    pub plugin: &'a str,
    /// The page's address with its query: forms and actions post here.
    pub action: &'a str,
    /// Starts every popover id drawn with this context.
    pub prefix: String,
    /// Whether the app reads the characters pilots register for it (user
    /// scopes), so its card grids may start with Tether's Register
    /// Character card.
    pub registers: bool,
    /// The site's origin (as the direct join link's): links to share start
    /// with it.
    pub site: &'a str,
    /// For a viewer who may add the app's owners: the page's link path,
    /// which Add owner's login comes back to, and the scopes EVE asks for.
    /// `None` draws no Add owner.
    pub owner_back: Option<(String, Vec<String>)>,
    /// On the Dashboard, footers for the cards of the viewer's own
    /// characters, by character id.
    feet: Option<&'a std::collections::HashMap<i64, CardFoot>>,
    /// The page's forms an action opens in a popup.
    popups: std::collections::BTreeSet<String>,
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
            feet: None,
            popups: std::collections::BTreeSet::new(),
            next: std::cell::Cell::new(0),
        }
    }

    /// The page's forms that actions open in a popup.
    pub fn with_popups(mut self, popups: std::collections::BTreeSet<String>) -> Self {
        self.popups = popups;
        self
    }

    /// The popup a form of the page is drawn in, if an action opens it.
    fn popup(&self, form: &str) -> Option<String> {
        self.popups
            .contains(form)
            .then(|| format!("{}-popup-{form}", self.prefix))
    }

    /// For a viewer who may add owners, on the page at `back`, the login
    /// asking for `scopes` (see `owner_back`).
    pub fn adding_owners(mut self, back: Option<(String, Vec<String>)>) -> Self {
        self.owner_back = back;
        self
    }

    /// For an app with user scopes (see `registers`).
    pub fn registering(mut self, registers: bool) -> Self {
        self.registers = registers;
        self
    }

    /// With footers for the viewer's own characters' cards (`feet`).
    fn with_feet(mut self, feet: &'a std::collections::HashMap<i64, CardFoot>) -> Self {
        self.feet = Some(feet);
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
        // Opening a popup, its confirmation is the popup's lead line.
        popover: action
            .confirm
            .as_ref()
            .filter(|_| ctx.popup(&action.form).is_none())
            .map(|_| ctx.next_id()),
        opens: ctx.popup(&action.form),
        fields_json: serde_json::Value::Object(
            action
                .fields
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        )
        .to_string(),
    }
}

fn value(ctx: &Ctx, value: &Value) -> ValueView {
    let plugin = ctx.plugin;
    let plain = |text: String| ValueView {
        text,
        title: None,
        href: None,
        primary: false,
        download: false,
        badge: None,
        status: None,
        mono: false,
        entity: None,
        countdown: None,
        progress: None,
        actions: Vec::new(),
        share: None,
        add_owner: None,
        levels: None,
        composition: None,
        defenses: None,
        unit: None,
        sort: None,
    };
    let mono = |text: String| ValueView {
        mono: true,
        ..plain(text)
    };
    match value {
        Value::Text(text) => plain(text.clone()),
        Value::Number(n) => ValueView {
            sort: Some(n.to_string()),
            ..mono(grouped(*n))
        },
        Value::Isk(amount) => ValueView {
            title: Some(format!("{} ISK", grouped(amount.trunc() as i64))),
            unit: Some("ISK"),
            sort: Some(format!("{amount}")),
            ..mono(short_isk(*amount))
        },
        Value::Time(text) => match utc(text) {
            // Checked as RFC 3339 by the host already.
            Some(at) => ValueView {
                title: Some(format!("{} EVE", at.format("%Y-%m-%d %H:%M:%S"))),
                unit: Some("EVE"),
                sort: Some(at.timestamp().to_string()),
                ..mono(at.format("%Y-%m-%d %H:%M").to_string())
            },
            None => mono(text.clone()),
        },
        Value::Badge(badge) => match badge_kind(&badge.tone) {
            Ok(variant) => ValueView {
                badge: Some(variant),
                ..plain(badge.label.clone())
            },
            Err(tone) => ValueView {
                status: Some(tone),
                ..plain(badge.label.clone())
            },
        },
        Value::Link(link) => ValueView {
            href: Some(page_href(plugin, &link.path)),
            primary: link.primary,
            download: link.path.starts_with("downloads/"),
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
            add_owner: ctx.owner_back.as_ref().map(|(back, scopes)| AddOwnerView {
                label: if label.trim().is_empty() {
                    "Add data source".to_owned()
                } else {
                    label.clone()
                },
                plugin: plugin.to_owned(),
                back: back.clone(),
                scopes: scopes.clone(),
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
                    sort: Some(at.timestamp().to_string()),
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
        Value::Levels(l) => {
            let view = super::plugin_visuals::levels(l);
            ValueView {
                levels: Some(view),
                ..plain(String::new())
            }
        }
        Value::Composition(c) => ValueView {
            composition: Some(super::plugin_visuals::composition(c)),
            ..plain(String::new())
        },
        Value::Defenses(d) => ValueView {
            defenses: Some(super::plugin_visuals::defenses(d)),
            ..plain(String::new())
        },
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
                variant: badge_kind(&b.tone).unwrap_or(""),
                status: badge_kind(&b.tone).err(),
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
                .map(|s| {
                    // A badge in a stat card is its number (or word) in
                    // the tone's colour, not a box.
                    let (value, tone) = match &s.value {
                        Value::Badge(badge) => (
                            ValueView {
                                mono: true,
                                ..value(ctx, &Value::Text(badge.label.clone()))
                            },
                            match badge.tone {
                                Tone::Warning | Tone::Accent => Some("warn"),
                                Tone::Danger => Some("danger"),
                                Tone::Success | Tone::Neutral => None,
                            },
                        ),
                        other => (value(ctx, other), None),
                    };
                    StatView {
                        label: s.label.clone(),
                        value,
                        caption: s.caption.clone(),
                        tone,
                    }
                })
                .collect(),
        ),
        Section::Table(table) => SectionView::Table(TableView {
            pager: None,
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
            register: (grid.register && ctx.registers)
                .then(|| format!("/register?app={}", ctx.plugin)),
            columns: grid
                .items
                .iter()
                .find(|card| !card.profile.facts.is_empty())
                .map(|card| {
                    card.profile
                        .facts
                        .iter()
                        .map(|(label, _)| label.clone())
                        .collect()
                })
                .unwrap_or_default(),
            items: grid
                .items
                .iter()
                .map(|card| CardItemView {
                    profile: profile(ctx, &card.profile),
                    href: card.link.as_deref().map(|path| page_href(ctx.plugin, path)),
                    foot: ctx
                        .feet
                        .filter(|_| matches!(card.profile.subject.kind, EntityKind::Character))
                        .and_then(|feet| feet.get(&card.profile.subject.id))
                        .cloned(),
                })
                .collect(),
        }),
        Section::Timeline(t) => SectionView::Timeline(super::plugin_visuals::timeline(
            ctx.plugin,
            t,
            chrono::Utc::now(),
        )),
        Section::Code(code) => SectionView::Code(CodeView {
            title: code.title.clone(),
            text: code.text.clone(),
            copy_label: code.copy_label.clone().unwrap_or_else(|| "Copy".to_owned()),
        }),
        Section::Form(form) => SectionView::Form(FormView {
            id: form.id.clone(),
            popup: ctx.popup(&form.id),
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
                        blank: false,
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
                            view.blank =
                                !f.required && !input.options.iter().any(|c| c.value.is_empty());
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
    /// The app's name, its icon (DESIGN.md, Page header) and its first
    /// page's address.
    pub app_name: String,
    pub icon: &'static str,
    pub app_href: String,
    /// The sidebar section the app's link sits in by default, for the
    /// eyebrow when a page is named after the app itself.
    pub section: &'static str,
    /// The app's Manage pages (DESIGN.md, App shell), for those who run it:
    /// Manage opens the first.
    pub manage: Vec<TabLink>,
    /// The page is one of the app's Manage pages: its eyebrow says so and
    /// its bar shows the Manage pages.
    pub in_manage: bool,
    /// The page's own pages (a record's), as view chips under the bar.
    pub sub_links: Vec<TabLink>,
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
    /// The app's id, when the page refreshes once the app's data changed
    /// (`assets/live.js`).
    pub live: Option<String>,
    /// The page's address with its query: what a reload fetches.
    pub href: String,
    /// The app's owners (Add owner), drawn by the host.
    pub owners: Option<super::plugin_access::Owners>,
    /// Every view is audited: kept out of htmx's history cache, so back
    /// and forward ask the server (and are recorded) again.
    pub audited: bool,
    /// The character audit's main page drawn as the Dashboard: its header
    /// is the Dashboard's.
    pub home: Option<HomeView>,
}

/// The Dashboard's header over the character audit (DESIGN.md,
/// Dashboard): the account's state and its first groups.
pub struct HomeView {
    pub state_style: &'static str,
    pub state_name: String,
    pub groups: Vec<String>,
    /// Groups beyond those shown.
    pub more_groups: usize,
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
    /// The character they act as, if not their main (`identity.acting`).
    acting: Option<Character>,
    path: String,
    query: Vec<(String, String)>,
    tab: usize,
    /// Its tables' pages (`_pN`), which the app never sees.
    table_pages: Vec<(usize, usize)>,
    /// The page's own address, with its query: forms post back here.
    href: String,
    owners: Option<super::plugin_access::Owners>,
    /// The site's origin, for links to share.
    site: String,
    /// The app's views, Manage pages and action for this viewer.
    frame: Frame,
    /// The character audit's main page, drawn as the Dashboard.
    home: Option<Home>,
}

/// Reads what the Dashboard draws around the character audit's main page:
/// the footers under the account's characters and the account's groups.
async fn with_home(state: &AppState, opened: &mut Opened) -> Result<(), PageError> {
    if let Some(home) = &mut opened.home {
        home.feet = super::card_feet(state, opened.account).await?;
        home.groups = tether_db::groups::names_for(&state.db, opened.account).await?;
    }
    Ok(())
}

/// The Dashboard's address, and the page name that marks its sidebar
/// link.
const DASHBOARD: &str = "/dashboard";
const DASHBOARD_ACTIVE: &str = "profile";

/// The character audit's main page drawn as the Dashboard (DESIGN.md,
/// Dashboard): Tether's footers under the account's characters, and its
/// state and groups under the title (`with_home` reads the footers and
/// groups).
struct Home {
    feet: std::collections::HashMap<i64, CardFoot>,
    state_style: &'static str,
    state_name: String,
    groups: Vec<String>,
}

/// The app's sidebar link a page sits under: the `[[navigation]]` entry
/// covering it (the longest path), else the app's main page.
fn nav_path<'a>(manifest: &'a manifest::Manifest, path: &str) -> &'a str {
    manifest
        .navigation
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|p| {
            p.is_empty()
                || path == *p
                || path
                    .strip_prefix(*p)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|p| p.len())
        .unwrap_or("")
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
    // Browser sessions only. The token layer keeps personal access tokens
    // off /plugins already; this holds even if a route ever let one
    // through, since apps would take it for its account (a superuser's,
    // say), whatever the token's scopes.
    if session.token_scopes.is_some() {
        return Err(missing());
    }
    manifest::check_id(id).map_err(|_| missing())?;
    let running = state.plugins.running(id).ok_or_else(missing)?;
    page_rules::check_link_path(path).map_err(|_| missing())?;
    let perms = tether_db::permissions::effective(&state.db, session.account).await?;
    let access = running.manifest.page_access(path);
    // Whether the Blacklist matters here: for this page, or the frame's
    // pages that only ask for a signed-in account.
    let signed_in_pages = running
        .manifest
        .pages
        .iter()
        .any(|rule| rule.permission.is_none());
    let blacklisted = (access == manifest::PageAccess::SignedIn || signed_in_pages)
        && tether_db::states::account_state(&state.db, session.account)
            .await?
            .is_some_and(|s| s.is_blacklist());
    let may = |page: &str| {
        tether_web_core::plugins::may_open(&running.manifest.page_access(page), blacklisted, |p| {
            perms.contains(p)
        })
    };
    if !may(path) {
        return Err(missing());
    }
    let mut frame = frame(&running.manifest, path, may);
    let viewer = viewer(state, &session, &running, &perms).await?;
    // Change character: one of the account's own, not the main
    // (`identity.acting`).
    let acting = session
        .acting
        .filter(|id| *id != viewer.main.id)
        .and_then(|id| viewer.characters.iter().find(|c| c.id == id).cloned());
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
    let table_pages: Vec<(usize, usize)> = pairs
        .iter()
        .filter_map(|(k, v)| Some((table_page_key(k)?, v.parse().ok()?)))
        .collect();
    let query: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(k, _)| k != TAB && table_page_key(k).is_none())
        .collect();
    // Not "plugins": that's the admin page; the plugin's own link is
    // marked through active_href, on every page of the app.
    let loaded = load(state, &session, "plugin-page").await?;
    let mut shell = loaded.shell;
    // The character audit's pages belong to the Dashboard (DESIGN.md,
    // Dashboard): it stays marked, and the app's main page is the
    // Dashboard itself.
    let audit = id == super::CHARACTER_AUDIT && shell.character_audit;
    let home = if audit {
        shell.active = DASHBOARD_ACTIVE;
        let main = page_href(id, "");
        for view in &mut frame.views {
            if view.href == main {
                view.href = DASHBOARD.to_owned();
            }
        }
        // Its footers and groups are read by `with_home`, once the page
        // is to be drawn (not for the redirect, nor over the budget).
        path.is_empty().then(|| Home {
            feet: std::collections::HashMap::new(),
            state_style: loaded.state.style(),
            state_name: loaded.state.name,
            groups: Vec::new(),
        })
    } else {
        shell.active_href = page_href(id, nav_path(&running.manifest, path));
        None
    };
    let href = page_href(id, path);
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
            acting,
            path: path.to_owned(),
            query,
            tab,
            table_pages,
            href,
            owners,
            site: state.site.origin().to_owned(),
            frame,
            home,
        },
    ))
}

/// What an app's frame shows this viewer (DESIGN.md, App shell): its
/// views, its Manage pages (Settings first) and its primary action, each
/// only if they may open its page, the one being shown marked. Empty for
/// an app that declares no views: its pages bring their own links.
#[derive(Default)]
struct Frame {
    declared: bool,
    views: Vec<TabLink>,
    manage: Vec<TabLink>,
    /// The page shown is one of the Manage pages (or under one).
    in_manage: bool,
    action: Option<TabLink>,
}

fn frame(manifest: &manifest::Manifest, path: &str, may: impl Fn(&str) -> bool) -> Frame {
    if manifest.views.is_empty() {
        return Frame::default();
    }
    let id = &manifest.plugin.id;
    let settings = manifest
        .pages
        .iter()
        .any(|rule| rule.path == manifest::SETTINGS_PATH)
        && may(manifest::SETTINGS_PATH);
    let settings_link = manifest::PageLink {
        label: "Settings".to_owned(),
        path: manifest::SETTINGS_PATH.to_owned(),
    };
    let manage: Vec<&manifest::PageLink> = settings
        .then_some(&settings_link)
        .into_iter()
        .chain(manifest.manage.iter().filter(|l| may(&l.path)))
        .collect();
    // The page shown belongs to the view or Manage page with the longest
    // path covering it: the main page only itself, so a page under no
    // view (a record, the action's own page) marks none.
    let covers = |p: &str| {
        path == p
            || (!p.is_empty()
                && path
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with('/')))
    };
    let current = manifest
        .views
        .iter()
        .chain(manage.iter().copied())
        .map(|l| l.path.as_str())
        .filter(|p| covers(p))
        .max_by_key(|p| p.len());
    let link = |l: &manifest::PageLink| TabLink {
        label: l.label.clone(),
        href: page_href(id, &l.path),
        current: current == Some(l.path.as_str()),
    };
    let in_manage = manage.iter().any(|l| current == Some(l.path.as_str()));
    Frame {
        declared: true,
        views: manifest
            .views
            .iter()
            .filter(|l| may(&l.path))
            .map(link)
            .collect(),
        manage: manage.into_iter().map(link).collect(),
        in_manage,
        action: manifest
            .action
            .as_ref()
            .filter(|a| a.path != path && may(&a.path))
            .map(link),
    }
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
            // Plugins know three states; the Blacklist counts as Guest to
            // them (it's never treated as a member).
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
}

impl Via {
    fn as_str(self) -> &'static str {
        match self {
            Via::Page => "page",
            Via::Reload => "reload",
            Via::Form => "form",
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
            opened.acting.clone(),
            &Default::default(),
        )
        .await
    {
        Ok(rendered) => {
            record_logs(&state.db, id, &source(&opened.path), &rendered.logs).await;
            let mut page = rendered.page;
            fill_names(state, &mut page).await;
            Ok(page)
        }
        Err(err) => Err(render_error(state, opened, err).await),
    }
}

/// The page's path, for Add owner to come back to, and the scopes its
/// login asks for, if the viewer may add the app's owners (a browser
/// session holding the app's add permission).
fn owner_back(opened: &Opened) -> Option<(String, Vec<String>)> {
    opened
        .owners
        .as_ref()
        .filter(|o| o.can_offer)
        .map(|o| (opened.path.clone(), o.scopes.clone()))
}

/// Every entity on a page, to look at or rename.
fn each_entity(page: &mut Page, f: &mut impl FnMut(&mut Entity)) {
    fn value(v: &mut Value, f: &mut impl FnMut(&mut Entity)) {
        if let Value::Entity(e) = v {
            f(e);
        }
    }
    fn profile(p: &mut Profile, f: &mut impl FnMut(&mut Entity)) {
        f(&mut p.subject);
        if let Some(e) = p.corporation.as_mut() {
            f(e);
        }
        if let Some(e) = p.alliance.as_mut() {
            f(e);
        }
        for (_, v) in &mut p.facts {
            value(v, f);
        }
    }
    let sections = page
        .sections
        .iter_mut()
        .chain(page.tabs.iter_mut().flat_map(|t| t.sections.iter_mut()));
    for s in sections {
        match s {
            Section::Stats(stats) => stats.iter_mut().for_each(|s| value(&mut s.value, f)),
            Section::Table(table) => table.rows.iter_mut().flatten().for_each(|v| value(v, f)),
            Section::Card(card) => card.fields.iter_mut().for_each(|(_, v)| value(v, f)),
            Section::Profile(p) => profile(p, f),
            Section::Cards(grid) => grid
                .items
                .iter_mut()
                .for_each(|c| profile(&mut c.profile, f)),
            Section::Text(_) | Section::Form(_) | Section::Code(_) | Section::Timeline(_) => {}
        }
    }
}

/// Whether a page shows another pilot: a character that isn't one of the
/// viewer's own. Only such pages carry the watermark (DESIGN.md,
/// Watermark): a screenshot of other pilots' data names who took it.
fn shows_others(page: &Page, viewer: &Viewer) -> bool {
    let mut page = page.clone();
    let mut others = false;
    each_entity(&mut page, &mut |e| {
        if e.kind == EntityKind::Character && !viewer.characters.iter().any(|c| c.id == e.id) {
            others = true;
        }
    });
    others
}

/// An entity an app knows only by its id so far (its name not read yet),
/// which it names with the id itself.
fn unnamed(e: &Entity) -> bool {
    let name = e.name.trim();
    // Tether's names cache holds characters, corporations, alliances and
    // factions: never an item's.
    !matches!(e.kind, EntityKind::Type) && e.id > 0 && (name.is_empty() || name == e.id.to_string())
}

/// Names for entities an app doesn't know the name of yet (DESIGN.md: no
/// raw ids): from Tether's own names cache where it has them, else
/// "Unknown corporation" and the like.
async fn fill_names(state: &AppState, page: &mut Page) {
    let mut ids = Vec::new();
    each_entity(page, &mut |e| {
        if unnamed(e) {
            ids.push(e.id);
        }
    });
    if ids.is_empty() {
        return;
    }
    let names = match tether_db::compliance::cached_names(&state.db, &ids).await {
        Ok(names) => names,
        Err(err) => {
            tracing::warn!(error = %err, "looking up entity names");
            Default::default()
        }
    };
    each_entity(page, &mut |e| {
        if unnamed(e) {
            e.name = names.get(&e.id).cloned().unwrap_or_else(|| {
                match e.kind {
                    EntityKind::Character => "Unknown character",
                    EntityKind::Corporation => "Unknown corporation",
                    EntityKind::Alliance => "Unknown alliance",
                    EntityKind::Faction | EntityKind::Type => "Unknown faction",
                }
                .to_owned()
            });
        }
    });
}

/// Draws a page, or with `alone` only its content: for a live page
/// reloading itself, a tab, or a post answered in place.
fn draw(
    opened: Opened,
    page: &Page,
    status: StatusCode,
    error: Option<String>,
    alone: bool,
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
    let mut ctx = Ctx::new(&id, &opened.href, "page".to_owned(), &opened.site)
        .registering(!opened.running.manifest.capabilities.esi.user.is_empty())
        .adding_owners(owner_back(&opened))
        .with_popups(page_rules::popup_forms(page));
    if let Some(home) = &opened.home {
        ctx = ctx.with_feet(&home.feet);
    }
    let sections: Vec<SectionView> =
        arrange(page.sections.iter().map(|s| section(&ctx, s)).collect());
    let tab_sections: Vec<SectionView> = page
        .tabs
        .get(tab)
        .map(|chosen| arrange(chosen.sections.iter().map(|s| section(&ctx, s)).collect()))
        .unwrap_or_default();
    // Long tables a page at a time, each keeping the others' pages.
    let table_href = |index: usize, number: usize| {
        let mut parts = base_query.clone();
        if tab > 0 {
            parts.push(format!("{TAB}={tab}"));
        }
        for (n, p) in &opened.table_pages {
            if *n != index {
                parts.push(format!("{TABLE_PAGE}{n}={p}"));
            }
        }
        parts.push(format!("{TABLE_PAGE}{index}={number}"));
        format!("{}?{}", page_href(&id, &opened.path), parts.join("&"))
    };
    let mut sections = sections;
    let mut tab_sections = tab_sections;
    let mut next_index = 0;
    paginate(
        &mut sections,
        &mut next_index,
        &opened.table_pages,
        &table_href,
    );
    paginate(
        &mut tab_sections,
        &mut next_index,
        &opened.table_pages,
        &table_href,
    );
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
    // An app's settings open from its Administration page, not from its
    // own header (configuring an app is an admin's job); once there, its
    // settings pages link to each other.
    let in_settings = tether_plugins::manifest::is_settings(&opened.path);
    // The page's own primary link (a record's Edit, say) is its action;
    // else the app's, from its manifest.
    let page_action: Vec<TabLink> = page
        .links
        .iter()
        .filter(|l| l.primary)
        .map(header_link)
        .collect();
    let frame = &opened.frame;
    // A page's own other links, in an app with a frame: the record's own
    // pages (a character's Skills, Assets, Wallet), as view chips under
    // the bar.
    let sub_links: Vec<TabLink> = if frame.declared {
        page.links
            .iter()
            .filter(|l| !l.primary)
            .map(header_link)
            .collect()
    } else {
        Vec::new()
    };
    let (links, buttons, manage) = if frame.declared {
        let bar = if frame.in_manage {
            frame.manage.clone()
        } else {
            frame.views.clone()
        };
        // One view alone needs no bar.
        let bar = if bar.len() > 1 { bar } else { Vec::new() };
        let buttons = if !page_action.is_empty() {
            page_action
        } else if frame.in_manage {
            Vec::new()
        } else {
            frame.action.clone().into_iter().collect()
        };
        let manage = if frame.in_manage {
            Vec::new()
        } else {
            frame.manage.clone()
        };
        (bar, buttons, manage)
    } else {
        let links = page
            .links
            .iter()
            .filter(|l| {
                !l.primary && (in_settings || !tether_plugins::manifest::is_settings(&l.path))
            })
            .map(header_link)
            .collect();
        (links, page_action, Vec::new())
    };
    // The account's main, whichever character it acts as: a screenshot
    // of other pilots' data names who took it. Not on the Dashboard, nor on
    // pages with only the viewer's own (DESIGN.md, Watermark).
    let watermark = if opened.home.is_some() || !shows_others(page, &opened.viewer) {
        String::new()
    } else {
        format!(
            "Viewing as {} · {} EVE",
            opened.viewer.main.name,
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        )
    };
    let home = opened.home.as_ref().map(|h| HomeView {
        state_style: h.state_style,
        state_name: h.state_name.clone(),
        groups: h
            .groups
            .iter()
            .take(super::DASHBOARD_GROUPS)
            .cloned()
            .collect(),
        more_groups: h.groups.len().saturating_sub(super::DASHBOARD_GROUPS),
    });
    // The character audit's pages lead back to the Dashboard.
    let app_href = if id == super::CHARACTER_AUDIT && opened.shell.character_audit {
        DASHBOARD.to_owned()
    } else {
        page_href(&id, "")
    };
    let manifest = &opened.running.manifest;
    let section = manifest
        .navigation
        .first()
        .map(|entry| entry.section())
        .and_then(|name| {
            tether_web_core::menu::SECTIONS
                .iter()
                .find(|(n, _)| *n == name)
        })
        .map_or("Apps", |(_, label)| label);
    let content = ContentView {
        app_name: manifest.plugin.name.clone(),
        icon: if home.is_some() {
            "grid"
        } else {
            tether_web_core::plugins::icon_of(manifest)
        },
        app_href,
        section,
        manage,
        in_manage: opened.frame.in_manage,
        sub_links,
        title: if home.is_some() {
            "Dashboard".to_owned()
        } else {
            page.title.clone()
        },
        description: if home.is_some() {
            None
        } else {
            page.description.clone()
        },
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
        live: (error.is_none() && !audited).then(|| opened.running.manifest.plugin.id.clone()),
        error,
        watermark,
        href: opened.href.clone(),
        owners: opened.owners,
        audited,
        home,
    };
    let mut response = if alone {
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
        HeaderValue::from_static("HX-Request, HX-Target, HX-Trigger"),
    );
    if alone || audited {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// `GET /dashboard` for whoever may open the character audit's main page
/// (`Shell::character_audit`): that page, as the Dashboard (DESIGN.md,
/// Dashboard). `None` for everyone else, and whenever the app can't show
/// it now (failed, or the viewer opened too many of its pages): the
/// account's own Dashboard then, so an app's trouble never costs anyone
/// their Dashboard. The app's log says why (admins read it there).
pub async fn home(
    state: &AppState,
    session: CurrentSession,
    headers: &HeaderMap,
) -> Result<Option<Response>, PageError> {
    if state.plugins.running(super::CHARACTER_AUDIT).is_none() {
        return Ok(None);
    }
    let (session, mut opened) =
        match open(state, Some(session), super::CHARACTER_AUDIT, "", None).await {
            Ok(opened) => opened,
            // Not for this account, or no main yet.
            Err(err)
                if matches!(
                    err.0.status(),
                    StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
                ) =>
            {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
    if opened.home.is_none()
        || state
            .limits
            .plugin_pages
            .check(
                (session.account.0, super::CHARACTER_AUDIT.to_owned()),
                std::time::Instant::now(),
            )
            .is_err()
    {
        return Ok(None);
    }
    with_home(state, &mut opened).await?;
    let via = if is_reload(headers) {
        Via::Reload
    } else {
        Via::Page
    };
    Ok(match render_page(state, &opened, via).await {
        Ok(page) => Some(draw(
            opened,
            &page,
            StatusCode::OK,
            None,
            content_alone(headers),
        )),
        Err(_) => None,
    })
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

/// Whether htmx asked for the content alone: a live page reloading, or a
/// tab (its links target the content). Never for a history restore,
/// which puts back the whole page.
fn content_alone(headers: &HeaderMap) -> bool {
    is_reload(headers)
        || (super::is_htmx(headers)
            && headers
                .get("hx-target")
                .is_some_and(|v| v.as_bytes() == CONTENT_ID.as_bytes())
            && !headers.contains_key("hx-history-restore-request"))
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
    let alone = content_alone(&headers);
    let shown = async {
        let (session, mut opened) = open(&state, session, &id, &path, raw.as_deref()).await?;
        // The character audit's main page is the Dashboard: its address
        // shows it. Its content alone (a reload) still comes from here.
        if opened.home.is_some() && !alone {
            return Ok(Redirect::to(DASHBOARD).into_response());
        }
        if let Err(retry) = state
            .limits
            .plugin_pages
            .check((session.account.0, id.clone()), std::time::Instant::now())
        {
            return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
        }
        with_home(&state, &mut opened).await?;
        let via = if reload { Via::Reload } else { Via::Page };
        let page = render_page(&state, &opened, via).await?;
        Ok::<_, PageError>(draw(opened, &page, StatusCode::OK, None, alone))
    }
    .await;
    match shown {
        Err(err) if reload => Ok(reload_failed(err.0.status())),
        // A tab that can't be shown: the whole page says why, not a
        // page inside the content.
        Err(err) if alone => {
            let mut response = err.into_response();
            let headers = response.headers_mut();
            headers.insert("hx-retarget", HeaderValue::from_static("body"));
            headers.insert("hx-reswap", HeaderValue::from_static("innerHTML show:top"));
            Ok(response)
        }
        other => other,
    }
}

/// Where a form or an action on an app's page was posted from, and so
/// how the answer comes back (DESIGN.md, Page hygiene and state).
enum Posted {
    /// Without htmx: the whole page, as ever.
    Whole,
    /// From the page itself: its content, swapped in place.
    InPlace,
    /// From another page (the Dashboard, for the character audit): back
    /// there, in place.
    Elsewhere(String),
}

impl Posted {
    fn of(state: &AppState, headers: &HeaderMap, page: &str) -> Self {
        if !super::is_htmx(headers) {
            return Self::Whole;
        }
        match super::stay::current_page(state.site.origin(), headers) {
            Some(here) if here.split('?').next() == Some(page) => Self::InPlace,
            Some(here) => Self::Elsewhere(here),
            None => Self::Whole,
        }
    }

    /// Answers with `page` (the page as it is now, or the one the app
    /// answered with), `error` if the post was refused, and a toast.
    fn answer(
        &self,
        opened: Opened,
        page: &Page,
        status: StatusCode,
        error: Option<String>,
        toast: Toast,
    ) -> Response {
        match self {
            Self::Whole => draw(opened, page, status, error, false),
            Self::InPlace => {
                let mut response = draw(opened, page, status, error, true);
                let headers = response.headers_mut();
                headers.insert("hx-retarget", HeaderValue::from_static("#plugin-content"));
                headers.insert("hx-reswap", HeaderValue::from_static("outerHTML show:none"));
                headers.insert("hx-push-url", HeaderValue::from_static("false"));
                with_toast(response, toast)
            }
            Self::Elsewhere(_) if toast.is_problem() => {
                with_toast(StatusCode::NO_CONTENT.into_response(), toast)
            }
            Self::Elsewhere(here) => with_toast(Redirect::to(here).into_response(), toast),
        }
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
    headers: HeaderMap,
    posted: Vec<(String, String)>,
) -> Result<Response, PageError> {
    let from = Posted::of(&state, &headers, &page_href(&id, &path));
    let (session, mut opened) = open(&state, session, &id, &path, raw.as_deref()).await?;
    if let Err(retry) = state
        .limits
        .plugin_submits
        .check((session.account.0, id.clone()), std::time::Instant::now())
    {
        return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
    }
    with_home(&state, &mut opened).await?;
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
    let refused = |opened: Opened, status: StatusCode, problem: String| {
        let toast = Toast::problem(problem.clone());
        from.answer(opened, &page, status, Some(problem), toast)
    };
    // What the toast names: the button, or the form's.
    let popup = page_rules::popup_forms(&page).contains(&form_id);
    let (values, label) = if let Some(form) = page_rules::find_form(&page, &form_id)
        && popup
    {
        // A popup's post: the hidden fields of an action this page offers
        // this person now, then the form's own values, checked as any.
        let Some((action, own)) = page_rules::find_popup_action(&page, form, &values) else {
            return Ok(refused(
                opened,
                StatusCode::CONFLICT,
                "That form isn't on this page any more. Try again.".to_owned(),
            ));
        };
        match page_rules::check_submission(form, &own) {
            Ok(own) => {
                let mut values = action.fields.clone();
                values.extend(own);
                (values, form.submit_label.clone())
            }
            Err(problem) => {
                return Ok(refused(opened, StatusCode::UNPROCESSABLE_ENTITY, problem));
            }
        }
    } else if let Some(form) = page_rules::find_form(&page, &form_id) {
        match page_rules::check_submission(form, &values) {
            Ok(values) => (values, form.submit_label.clone()),
            Err(problem) => {
                return Ok(refused(opened, StatusCode::UNPROCESSABLE_ENTITY, problem));
            }
        }
    } else if let Some(action) = page_rules::find_action(&page, &form_id, &values) {
        (action.fields.clone(), action.label.clone())
    } else {
        return Ok(refused(
            opened,
            StatusCode::CONFLICT,
            "That form isn't on this page any more. Try again.".to_owned(),
        ));
    };
    let done = Toast::done(format!("{label} · done"));
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
            opened.acting.clone(),
            &Default::default(),
        )
        .await;
    match submitted {
        Ok(submitted) => {
            record_logs(&state.db, &id, &source(&opened.path), &submitted.logs).await;
            // Other people's open pages of the app show it too.
            if submitted.changed {
                tether_web_core::plugin_jobs::announce_change(&state.db, &id).await;
            }
            // Settings saved: the app reads with them now while there's
            // room in ESI's budget (Jay, 2026-10-05), not at its next tick.
            // In the background, best effort.
            if tether_plugins::manifest::is_settings(&opened.path)
                && matches!(submitted.result, SubmitResult::Redirect(_))
                && state.esi.has_room()
            {
                let (db, manifest) = (state.db.clone(), opened.running.manifest.clone());
                let actor = tether_db::audit::Actor::Account(opened.account);
                let gap = tether_web_core::plugin_jobs::triggered_gap(&state.esi, false);
                tokio::spawn(async move {
                    tether_web_core::plugin_jobs::run_app_schedules(
                        &db,
                        &manifest,
                        actor,
                        &serde_json::json!({ "reason": "settings_saved" }),
                        gap,
                    )
                    .await;
                });
            }
            match submitted.result {
                // Shown where the form was, under the same tab and query.
                SubmitResult::Page(mut page) => {
                    fill_names(&state, &mut page).await;
                    Ok(from.answer(opened, &page, StatusCode::OK, None, done))
                }
                SubmitResult::Redirect(to) => {
                    // A page of the app, with the query it chose (checked
                    // by the host: plain pairs, percent-encoded).
                    let (to_path, to_query) = to.split_once('?').unwrap_or((&to, ""));
                    let mut href = page_href(&id, to_path);
                    if !to_query.is_empty() {
                        href.push('?');
                        href.push_str(to_query);
                    }
                    // Back to this page: under the tab it was on.
                    if href.split('?').next() == Some(page_href(&id, &path).as_str())
                        && opened.tab > 0
                        && !to_query
                            .split('&')
                            .any(|p| p.starts_with(&format!("{TAB}=")))
                    {
                        href.push(if href.contains('?') { '&' } else { '?' });
                        href.push_str(&format!("{TAB}={}", opened.tab));
                    }
                    Ok(with_toast(Redirect::to(&href).into_response(), done))
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
    headers: HeaderMap,
    Form(posted): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    post(state, session, id, String::new(), raw, headers, posted).await
}

/// `POST /plugins/{id}/{*path}`
pub async fn post_sub(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, path)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    Form(posted): Form<Vec<(String, String)>>,
) -> Result<Response, PageError> {
    post(state, session, id, path, raw, headers, posted).await
}

/// `GET /plugins/{id}/downloads/{name}`: a file the app offers (its
/// `downloads`, as aa-memberaudit's data exports), for holders of the
/// app's permission named with it, gated by the main as the app's pages
/// are. Written by the host from the app's rows, streamed from Postgres;
/// each download audited.
pub async fn download(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    // 404 whether it's missing or just not for this account, as pages.
    let missing = || AppError::not_found("No such download.");
    manifest::check_id(&id).map_err(|_| missing())?;
    state
        .plugins
        .running(&id)
        .filter(|r| r.manifest.capabilities.downloads)
        .ok_or_else(missing)?;
    let file = tether_db::downloads::files(&state.db, &id)
        .await?
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(missing)?;
    let held = tether_db::permissions::effective(&state.db, session.account).await?;
    if !held.contains(&format!("plugin.{id}.{}", file.permission)) {
        return Err(missing().into());
    }
    tether_db::accounts::get(&state.db, session.account)
        .await?
        .and_then(|a| a.main)
        .ok_or_else(|| AppError::bad_request("Choose a main character first (Change Main)."))?;
    if let Err(retry) = state
        .limits
        .plugin_pages
        .check((session.account.0, id.clone()), std::time::Instant::now())
    {
        return Err(AppError::too_many_requests(retry.as_secs().max(1)).into());
    }
    tether_db::audit::record(
        &state.db,
        tether_db::audit::Actor::Account(session.account),
        "plugin.download",
        Some(&format!("plugin:{id}")),
        serde_json::json!({ "name": file.name, "rows": file.rows }),
    )
    .await
    .map_err(AppError::from)?;
    let parts = tether_web_core::plugin_downloads::PartsStream::new(state.db.clone(), &id, &file);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.csv\"", file.name),
            ),
            (header::CONTENT_LENGTH, file.bytes.to_string()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        axum::body::Body::from_stream(parts),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(title: &str, columns: &[&str]) -> SectionView {
        SectionView::Table(TableView {
            pager: None,
            title: Some(title.to_owned()),
            columns: columns
                .iter()
                .map(|c| ColumnView {
                    label: (*c).to_owned(),
                    numeric: false,
                })
                .collect(),
            rows: Vec::new(),
            empty: None,
        })
    }

    fn code(title: &str) -> SectionView {
        SectionView::Code(CodeView {
            title: Some(title.to_owned()),
            text: String::new(),
            copy_label: "Copy".to_owned(),
        })
    }

    /// Each view's shape: `T:title`, `S[a,b]` for tables drawn as one,
    /// `R(..)` for a row, `C:title` for code, `X` for text.
    fn shape(views: &[SectionView]) -> Vec<String> {
        views
            .iter()
            .map(|v| match v {
                SectionView::Table(t) => format!("T:{}", t.title.clone().unwrap_or_default()),
                SectionView::Tables(set) => format!(
                    "S[{}]",
                    set.groups
                        .iter()
                        .map(|g| g.title.clone().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                SectionView::Row(members) => format!("R({})", shape(members).join(",")),
                SectionView::Code(c) => format!("C:{}", c.title.clone().unwrap_or_default()),
                _ => "X".to_owned(),
            })
            .collect()
    }

    #[test]
    fn long_tables_are_paged_each_on_its_own() {
        let long = |n: usize| {
            let mut t = table("Long", &["A"]);
            if let SectionView::Table(t) = &mut t {
                t.rows = (0..n).map(|_| Vec::new()).collect();
            }
            t
        };
        let mut views = vec![long(60), long(10), long(26)];
        let href = |n: usize, p: usize| format!("?_p{n}={p}");
        let mut next = 0;
        paginate(&mut views, &mut next, &[(0, 2), (2, 9)], &href);
        let pager = |v: &SectionView| match v {
            SectionView::Table(t) => (
                t.rows.len(),
                t.pager
                    .as_ref()
                    .map(|p| (p.from, p.to, p.total, p.previous.clone(), p.next.clone())),
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            pager(&views[0]),
            (
                25,
                Some((26, 50, 60, Some("?_p0=1".into()), Some("?_p0=3".into())))
            )
        );
        // Short: whole, no pager.
        assert_eq!(pager(&views[1]), (10, None));
        // A page past the end is the last.
        assert_eq!(
            pager(&views[2]),
            (1, Some((26, 26, 26, Some("?_p2=1".into()), None)))
        );
        assert_eq!(next, 3);

        // Merged tables: each its own pages.
        let cols = ["A", "B", "C", "D"];
        let rows = |title: &str, n: usize| {
            let mut t = table(title, &cols);
            if let SectionView::Table(t) = &mut t {
                t.rows = (0..n).map(|_| Vec::new()).collect();
            }
            t
        };
        let mut views = arrange(vec![rows("High", 30), rows("Mid", 5)]);
        let mut next = 0;
        paginate(&mut views, &mut next, &[(0, 2)], &href);
        let SectionView::Tables(set) = &views[0] else {
            panic!("not merged: {:?}", shape(&views));
        };
        assert_eq!(set.groups[0].rows.len(), 5);
        assert_eq!(
            set.groups[0]
                .pager
                .as_ref()
                .map(|p| (p.from, p.to, p.total)),
            Some((26, 30, 30))
        );
        assert!(set.groups[1].pager.is_none());
        assert_eq!(next, 2);
    }

    #[test]
    fn tables_line_up_and_narrow_sections_pair_up() {
        let slots = ["Item", "Charge", "Count", "State"];
        let arranged = arrange(vec![
            table("High", &slots),
            table("Mid", &slots),
            table("Drones", &slots),
            code("EFT"),
            code("Buy All"),
            table("Doctrines", &["Doctrine", "About"]),
            table("Skills", &["Skill", "Level"]),
            SectionView::Text("note".to_owned()),
            table("Pilots", &["Pilot", "Can fly", "Missing"]),
            table("Other", &["A", "B", "C", "D"]),
            // Three columns alike: side by side, not one table.
            table("Fresh moons", &["Moon", "Structure", "Popped"]),
            table("Old moons", &["Moon", "Structure", "Popped"]),
        ]);
        assert_eq!(
            shape(&arranged),
            [
                "S[High,Mid,Drones]",
                "R(C:EFT,C:Buy All)",
                "R(T:Doctrines,T:Skills)",
                "X",
                "T:Pilots",
                "T:Other",
                "R(T:Fresh moons,T:Old moons)",
            ]
        );
        // Code and tables don't pair with each other; one alone stays wide.
        let arranged = arrange(vec![
            table("Skills", &["Skill", "Level"]),
            code("EFT"),
            table("Untitled", &["Item", "Charge", "Count", "State"]),
        ]);
        assert_eq!(shape(&arranged), ["T:Skills", "C:EFT", "T:Untitled"]);
    }

    #[test]
    fn isk_and_numbers_read_well() {
        assert_eq!(short_isk(1_240_000_000.0), "1.24b");
        assert_eq!(short_isk(350_200_000.0), "350.2m");
        assert_eq!(short_isk(12_500.0), "12.5k");
        assert_eq!(short_isk(999.0), "999");
        assert_eq!(short_isk(-2_000_000.0), "-2m");
        assert_eq!(short_isk(-0.0), "0");
        assert_eq!(short_isk(-0.3), "0");
        assert_eq!(grouped(1_240_000_000), "1,240,000,000");
        assert_eq!(grouped(-1234), "-1,234");
        assert_eq!(grouped(12), "12");
    }

    #[test]
    fn countdowns_read_as_live_js_writes_them() {
        assert_eq!(
            countdown_text(2 * 86_400 + 4 * 3_600 + 13 * 60 + 9),
            "T\u{2212} 2d 04h"
        );
        assert_eq!(countdown_text(19 * 3_600 + 40 * 60), "T\u{2212} 19h 40m");
        assert_eq!(countdown_text(4 * 3_600 + 3 * 60), "T\u{2212} 4h 03m");
        assert_eq!(countdown_text(13 * 60 + 5), "T\u{2212} 13m 05s");
        assert_eq!(countdown_text(45), "T\u{2212} 45s");
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
        assert_eq!(drawn(true).as_deref(), Some("/register?app=acme.x"));
        assert_eq!(drawn(false), None);
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
            .adding_owners(
                back.map(|b| (b.to_owned(), vec!["esi-fleets.read_fleet.v1".to_owned()])),
            )
        };
        let label = Value::AddOwner("Log in with the fleet boss".to_owned());
        assert!(value(&ctx(None), &label).add_owner.is_none());
        let drawn = value(&ctx(Some("links/create")), &label)
            .add_owner
            .expect("drawn");
        assert_eq!(drawn.label, "Log in with the fleet boss");
        assert_eq!(drawn.plugin, "acme.fat");
        assert_eq!(drawn.back, "links/create");
        assert_eq!(drawn.scopes, vec!["esi-fleets.read_fleet.v1".to_owned()]);
        let unnamed = value(&ctx(Some("")), &Value::AddOwner(" ".to_owned()))
            .add_owner
            .expect("drawn");
        assert_eq!(unnamed.label, "Add data source");
    }
}
