//! The command palette (DESIGN.md, Command palette): pages, pilots and
//! actions, only what the viewer may open. Built from the same reach as the
//! sidebar, Administration and the apps' frames (`pages::reach`), so it
//! never offers more than they do. Nothing is indexed or kept: each search
//! reads what the viewer may open now.

use std::collections::BTreeSet;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use tether_plugins::manifest;
use tether_web_core::admin_nav;
use tether_web_core::menu::{self, Section};
use tether_web_core::plugins::{icon_of, page_href};

use super::{CHARACTER_AUDIT, PageError, Reach, Shell, load, reach, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// Letters a pilot search needs, so one letter doesn't list everyone.
pub const PILOT_FROM: usize = 2;
/// Pilots shown at most.
const PILOTS: usize = 8;
/// Rows per group when every group is shown (All), and when one is.
const ALL_PAGES: usize = 8;
const ALL_ACTIONS: usize = 5;
const SCOPED: usize = 60;
/// What a search reads at most: longer words are cut.
const MAX_QUERY: usize = 100;

#[derive(Debug, Default, Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
    #[serde(default)]
    scope: String,
}

/// Which results a search shows: every group, or one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Pages,
    Pilots,
    Actions,
}

impl Scope {
    fn parse(text: &str) -> Self {
        match text {
            "pages" => Self::Pages,
            "pilots" => Self::Pilots,
            "actions" => Self::Actions,
            _ => Self::All,
        }
    }

    fn shows(self, group: Self) -> bool {
        self == Self::All || self == group
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Pages => "pages",
            Self::Pilots => "pilots",
            Self::Actions => "actions",
        }
    }
}

/// Where a result goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A page: a link (boosted, as any).
    Link { href: String, new_tab: bool },
    /// An action that posts: a form with no fields. `boost` false for one
    /// that leaves Tether (Add character goes to EVE's login).
    Post { action: String, boost: bool },
}

/// One result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    /// The muted line under it: where it is, or who.
    pub context: String,
    pub icon: &'static str,
    /// A pilot's portrait (their main's), instead of the icon.
    pub portrait: Option<i64>,
    pub target: Target,
}

impl Item {
    fn link(
        title: impl Into<String>,
        context: impl Into<String>,
        icon: &'static str,
        href: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            context: context.into(),
            icon,
            portrait: None,
            target: Target::Link {
                href: href.into(),
                new_tab: false,
            },
        }
    }

    fn href(&self) -> &str {
        match &self.target {
            Target::Link { href, .. } => href,
            Target::Post { action, .. } => action,
        }
    }

    /// The portrait's address, for the template.
    pub fn portrait_url(&self) -> Option<String> {
        self.portrait.and_then(super::portrait_url)
    }

    pub fn initials(&self) -> String {
        super::initials(&self.title)
    }
}

/// A group of results under its overline.
pub struct Group {
    pub label: &'static str,
    pub items: Vec<Item>,
}

/// The account menu's pages (the signed-in character's menu), for
/// everyone signed in.
const ACCOUNT_PAGES: &[(&str, &str, &str)] = &[
    ("Token Management", "/tokens", "lock"),
    ("Access tokens", "/dashboard/access-tokens", "scroll"),
    ("Notifications", "/notifications", "bell"),
    ("What's new", "/whats-new", "book"),
];

/// "Administration · Access" for an Administration page's address.
fn admin_context(href: &str) -> Option<String> {
    let page = admin_nav::PAGES.iter().find(|p| p.href == href)?;
    let group = admin_nav::GROUPS.iter().find(|g| g.key == page.group)?;
    Some(format!("Administration · {}", group.label))
}

fn section_label(name: &str) -> &'static str {
    menu::SECTIONS
        .iter()
        .find(|(key, _)| *key == name)
        .map_or("Apps", |(_, label)| label)
}

/// Every page the viewer may open, in the sidebar's order, then the
/// account menu's, Administration's and the apps' own views and Manage
/// pages. Each address once.
pub fn pages(reach: &Reach, apps: &[tether_web_core::plugins::Running]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    // The sidebar as the Menu arranges it: its labels, its custom links.
    sidebar_pages(&reach.menu, &mut items);
    // Pages the viewer may open but the sidebar doesn't show (hidden there
    // by an admin, or admin pages left unpinned).
    let available =
        tether_web_core::pages::menu_items(&reach.nav, reach.group_management, &reach.plugin_nav);
    for item in available {
        let context = section_label(item.section);
        items.push(Item::link(item.label, context, item.icon, item.href));
    }
    for (label, href, icon) in ACCOUNT_PAGES {
        items.push(Item::link(*label, "Account", icon, *href));
    }
    for group in admin_nav::listed(&reach.nav, "") {
        for page in group.pages {
            items.push(Item::link(page.label, "", page.icon, page.href));
        }
    }
    for running in apps {
        app_pages(reach, &running.manifest, &mut items);
    }
    // Administration's pages say where they are, wherever they came from.
    for item in &mut items {
        if let Some(context) = admin_context(item.href()) {
            item.context = context;
        }
    }
    let mut seen = BTreeSet::new();
    items.retain(|item| seen.insert(item.href().to_owned()));
    items
}

fn sidebar_pages(menu: &[Section], items: &mut Vec<Item>) {
    for section in menu {
        for node in &section.nodes {
            let nodes: Vec<(&menu::Node, String)> = if node.is_folder() {
                node.children
                    .iter()
                    .map(|c| (c, format!("{} · {}", section.label, node.label)))
                    .collect()
            } else {
                vec![(node, section.label.clone())]
            };
            for (node, context) in nodes {
                if node.is_folder() || node.href.is_empty() {
                    continue;
                }
                items.push(Item {
                    title: node.label.clone(),
                    context,
                    icon: node.icon,
                    portrait: None,
                    target: Target::Link {
                        href: node.href.clone(),
                        new_tab: node.new_tab,
                    },
                });
            }
        }
    }
}

/// An app's views and Manage pages the viewer may open, as its frame
/// shows them: its main page under the app's name, the rest under theirs.
fn app_pages(reach: &Reach, app: &manifest::Manifest, items: &mut Vec<Item>) {
    let id = &app.plugin.id;
    let name = &app.plugin.name;
    let icon = icon_of(app);
    let may = |path: &str| reach.may_open(&app.page_access(path));
    for (i, view) in app.views.iter().enumerate() {
        if !may(&view.path) {
            continue;
        }
        // The character audit's main page is the Dashboard.
        if reach.character_audit && id == CHARACTER_AUDIT && view.path.is_empty() {
            continue;
        }
        let item = if i == 0 {
            Item::link(
                name.clone(),
                view.label.clone(),
                icon,
                page_href(id, &view.path),
            )
        } else {
            Item::link(
                view.label.clone(),
                name.clone(),
                icon,
                page_href(id, &view.path),
            )
        };
        items.push(item);
    }
    if app.views.is_empty() {
        return;
    }
    let settings = app
        .pages
        .iter()
        .any(|rule| rule.path == manifest::SETTINGS_PATH)
        .then(|| manifest::PageLink {
            label: "Settings".to_owned(),
            path: manifest::SETTINGS_PATH.to_owned(),
        });
    for page in settings.iter().chain(app.manage.iter()) {
        if may(&page.path) {
            items.push(Item::link(
                page.label.clone(),
                format!("{name} · Manage"),
                icon,
                page_href(id, &page.path),
            ));
        }
    }
}

/// What the viewer may do from anywhere.
pub fn actions(reach: &Reach, apps: &[tether_web_core::plugins::Running]) -> Vec<Item> {
    let mut items = vec![Item {
        title: "Add character".to_owned(),
        context: "Log in with another character".to_owned(),
        icon: "plus",
        portrait: None,
        target: Target::Post {
            action: "/register/start".to_owned(),
            boost: false,
        },
    }];
    if reach.nav.groups {
        items.push(Item::link(
            "New group",
            "Administration · Access",
            "users",
            "/admin/groups#new-group",
        ));
    }
    if reach.nav.states {
        items.push(Item::link(
            "New state",
            "Administration · Access",
            "layers",
            "/admin/states#new-state",
        ));
    }
    for running in apps {
        let app = &running.manifest;
        if app.views.is_empty() {
            continue;
        }
        if let Some(action) = &app.action
            && reach.may_open(&app.page_access(&action.path))
        {
            items.push(Item::link(
                action.label.clone(),
                app.plugin.name.clone(),
                icon_of(app),
                page_href(&app.plugin.id, &action.path),
            ));
        }
    }
    items.push(Item {
        title: "Log out".to_owned(),
        context: "Account".to_owned(),
        icon: "log-out",
        portrait: None,
        target: Target::Post {
            action: "/auth/logout".to_owned(),
            boost: true,
        },
    });
    items
}

/// The items every word of `query` is in (title or context), titles that
/// start with it first, then titles holding every word; otherwise in their
/// own order. Everything for an empty query.
pub fn matching(items: Vec<Item>, query: &str) -> Vec<Item> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return items;
    }
    let mut ranked: Vec<(u8, Item)> = items
        .into_iter()
        .filter_map(|item| {
            let title = item.title.to_lowercase();
            let all = format!("{title} {}", item.context.to_lowercase());
            if !words.iter().all(|w| all.contains(w)) {
                return None;
            }
            let rank = if title.starts_with(&query) {
                0
            } else if words.iter().all(|w| title.contains(w)) {
                1
            } else {
                2
            };
            Some((rank, item))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, item)| item).collect()
}

/// Pilots whose characters' names hold `query`, for those who may open
/// Users: each the account, under the character found.
async fn pilots(state: &AppState, query: &str) -> Result<Vec<Item>, AppError> {
    if query.chars().count() < PILOT_FROM {
        return Ok(Vec::new());
    }
    let (rows, _) =
        tether_db::users::search(&state.db, query, None, tether_db::users::Status::All).await?;
    Ok(rows
        .into_iter()
        .take(PILOTS)
        .map(|row| {
            let mut context: Vec<String> = Vec::new();
            if row.matched.is_some() {
                context.push(format!("alt of {}", row.main_name));
            }
            context.extend(row.corporation.clone());
            context.extend(row.alliance.clone());
            if !row.active {
                context.push("Deactivated".to_owned());
            }
            let found_main = row.matched.is_none();
            Item {
                title: row.matched.unwrap_or(row.main_name),
                context: context.join(" · "),
                icon: "user",
                // A main's own portrait; an alt's isn't known here.
                portrait: (found_main && row.main_id > 0).then_some(row.main_id),
                target: Target::Link {
                    href: format!("/admin/users/{}", row.account_id),
                    new_tab: false,
                },
            }
        })
        .collect())
}

/// The results of one search, grouped.
pub async fn results(
    state: &AppState,
    reach: &Reach,
    query: &str,
    scope: Scope,
) -> Result<Vec<Group>, AppError> {
    let apps = state.plugins.all_running();
    let limit = |all: usize| if scope == Scope::All { all } else { SCOPED };
    let mut groups = Vec::new();
    if scope.shows(Scope::Pages) {
        let mut items = matching(pages(reach, &apps), query);
        items.truncate(limit(ALL_PAGES));
        groups.push(Group {
            label: "Pages",
            items,
        });
    }
    if scope.shows(Scope::Pilots) && reach.nav.users {
        groups.push(Group {
            label: "Pilots",
            items: pilots(state, query.trim()).await?,
        });
    }
    if scope.shows(Scope::Actions) {
        let mut items = matching(actions(reach, &apps), query);
        items.truncate(limit(ALL_ACTIONS));
        groups.push(Group {
            label: "Actions",
            items,
        });
    }
    groups.retain(|g| !g.items.is_empty());
    Ok(groups)
}

#[derive(Template)]
#[template(path = "palette_results.html")]
struct Results<'a> {
    groups: &'a [Group],
    /// Drawn for the palette (a listbox the input controls), or as a page.
    combobox: bool,
    /// Pilots need more letters than this search has.
    pilot_hint: bool,
}

#[derive(Template)]
#[template(path = "palette.html")]
struct PalettePage<'a> {
    shell: Shell,
    query: &'a str,
    scope: &'static str,
    groups: &'a [Group],
    pilot_hint: bool,
}

/// `GET /palette`: the palette's results as a fragment (its htmx search),
/// or, as a page of its own, the same search without script.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
    Query(search): Query<Search>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let query: String = search.q.trim().chars().take(MAX_QUERY).collect();
    let scope = Scope::parse(&search.scope);
    let fragment = headers.contains_key("hx-request") && !headers.contains_key("hx-boosted");
    if fragment {
        let reach = reach(&state, &session).await?;
        let groups = results(&state, &reach, &query, scope).await?;
        let pilot_hint =
            reach.nav.users && scope.shows(Scope::Pilots) && query.chars().count() < PILOT_FROM;
        return Ok(unkept(render(
            StatusCode::OK,
            &Results {
                groups: &groups,
                combobox: true,
                pilot_hint,
            },
        )));
    }
    let loaded = load(&state, &session, "palette").await?;
    let reach = reach(&state, &session).await?;
    let groups = results(&state, &reach, &query, scope).await?;
    let pilot_hint =
        reach.nav.users && scope.shows(Scope::Pilots) && query.chars().count() < PILOT_FROM;
    Ok(unkept(render(
        StatusCode::OK,
        &PalettePage {
            shell: loaded.shell,
            query: &query,
            scope: scope.key(),
            groups: &groups,
            pilot_hint,
        },
    )))
}

/// The results and the page share an address, so caches must tell them
/// apart; and neither is kept, since what they list (pilots among them)
/// is the viewer's alone and changes with a grant.
fn unkept(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::VARY,
        HeaderValue::from_static("HX-Request, HX-Boosted"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, context: &str) -> Item {
        Item::link(title, context, "grid", format!("/{title}"))
    }

    fn titles(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn every_word_must_match_and_titles_that_start_with_it_come_first() {
        let items = vec![
            item("Permissions Audit", "Administration · Access"),
            item("Groups", "Account"),
            item("Admin groups", "Administration · Access"),
            item("Group Management", "Account"),
            item("States", "Administration · Access"),
        ];
        let found = matching(items.clone(), "group");
        assert_eq!(
            titles(&found),
            ["Groups", "Group Management", "Admin groups"]
        );
        // Every word, in the title or its context.
        let found = matching(items.clone(), "access sta");
        assert_eq!(titles(&found), ["States"]);
        assert!(matching(items.clone(), "nothing here").is_empty());
        // Nothing typed: everything, in order.
        assert_eq!(matching(items.clone(), "  ").len(), items.len());
        // Case doesn't matter.
        assert_eq!(
            titles(&matching(items, "PERMISSIONS")),
            ["Permissions Audit"]
        );
    }

    #[test]
    fn scopes_parse_and_fall_back_to_all() {
        assert_eq!(Scope::parse("pilots"), Scope::Pilots);
        assert_eq!(Scope::parse("pages"), Scope::Pages);
        assert_eq!(Scope::parse("actions"), Scope::Actions);
        assert_eq!(Scope::parse(""), Scope::All);
        assert_eq!(Scope::parse("<script>"), Scope::All);
        assert!(Scope::All.shows(Scope::Pilots));
        assert!(!Scope::Pages.shows(Scope::Pilots));
    }

    #[test]
    fn admin_pages_say_where_they_are() {
        assert_eq!(
            admin_context("/admin/states").as_deref(),
            Some("Administration · Access")
        );
        assert_eq!(
            admin_context("/admin/system").as_deref(),
            Some("Administration · Instance")
        );
        assert_eq!(admin_context("/groups"), None);
    }
}
