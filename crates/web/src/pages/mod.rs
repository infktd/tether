//! Server-rendered pages (askama + Basecoat + htmx). Interactive pieces are
//! htmx requests to endpoints that return HTML fragments.

pub mod access_tokens;
pub mod admin;
pub mod assets;
pub mod autogroups;
pub mod blacklist;
pub mod compliance;
pub mod corpstats;
pub mod discord;
pub mod groups;
pub mod headers;
pub mod menu;
pub mod notifications;
pub mod permissions_audit;
pub mod pings;
pub mod plugin_access;
pub mod plugin_pages;
pub mod plugins;
pub mod setup;
pub mod states;
pub mod system;
pub mod tokens;
pub mod users;

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::states::State as AccessState;
use tether_db::{accounts, permissions, states as state_db};

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// An id however many references deep askama passes it.
pub trait CharacterId {
    fn id(&self) -> i64;
}

impl CharacterId for i64 {
    fn id(&self) -> i64 {
        *self
    }
}

impl<T: CharacterId + ?Sized> CharacterId for &T {
    fn id(&self) -> i64 {
        (**self).id()
    }
}

/// CCP's image server URL for a character portrait (64px, shown at 32 to
/// 36px). `None` for fixture characters, which have no portrait.
pub fn portrait_url(character_id: impl CharacterId) -> Option<String> {
    let id = character_id.id();
    (id > 0).then(|| format!("https://images.evetech.net/characters/{id}/portrait?size=64"))
}

/// Two-letter initials for characters without a portrait.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Renders a template, or a plain 500 if rendering itself fails.
pub(crate) fn render(status: StatusCode, template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(err) => AppError::internal(err).into_response(),
    }
}

pub(crate) fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some_and(|v| v == "true")
}

/// An error on a page: signed-out visitors go to the login page; everything
/// else gets the error page.
pub struct PageError(pub AppError);

impl From<AppError> for PageError {
    fn from(err: AppError) -> Self {
        Self(err)
    }
}

impl From<sqlx::Error> for PageError {
    fn from(err: sqlx::Error) -> Self {
        Self(err.into())
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    status: u16,
    title: &'a str,
    message: &'a str,
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let status = self.0.status();
        if status == StatusCode::UNAUTHORIZED {
            return Redirect::to("/login").into_response();
        }
        error_page(status, self.0.message())
    }
}

pub(crate) fn error_page(status: StatusCode, message: &str) -> Response {
    render(
        status,
        &ErrorPage {
            status: status.as_u16(),
            title: status.canonical_reason().unwrap_or("Error"),
            message,
        },
    )
}

/// Fallback for unknown paths.
pub async fn not_found() -> Response {
    error_page(StatusCode::NOT_FOUND, "There's nothing at this address.")
}

/// The signed-in character shown in the sidebar.
pub struct ShellUser {
    pub name: String,
    pub character_id: i64,
    pub state_name: String,
    pub is_owner: bool,
}

pub struct Shell {
    pub user: ShellUser,
    pub active: &'static str,
    /// Admin links the sidebar may show.
    pub nav: AdminNav,
    /// Plugin pages this account may open, from the plugins' manifests.
    pub plugin_nav: Vec<PluginNavLink>,
    /// The sidebar: what this account may see, as the Menu arranges it.
    pub menu: Vec<crate::menu::Section>,
    /// The plugin page being shown, to mark its sidebar link.
    pub active_href: String,
    /// The account's state when not every character is registered with
    /// its scopes: shown as a banner until they are (F11).
    pub not_compliant: Option<String>,
    /// The account lost its main (sold, or its token gone): Guest until
    /// the owner picks one (AA).
    pub no_main: bool,
    /// Pending requests, when the account may open Group Management.
    pub group_management: Option<i64>,
    /// Unread notifications, for the top bar.
    pub unread: i64,
    /// On Administration's pages: the rail of admin pages this account
    /// may open.
    pub admin_rail: Option<Vec<crate::admin_nav::Listed>>,
}

/// The sidebar items an account may see: built-in pages by its
/// permissions, then apps' pages.
pub(crate) fn menu_items(
    nav: &AdminNav,
    group_management: Option<i64>,
    plugin_nav: &[PluginNavLink],
) -> Vec<crate::menu::Available> {
    let may = |key: &str| match key {
        "dashboard" | "groups" => true,
        "group_management" => group_management.is_some(),
        "pings" => nav.pings,
        "administration" => nav.any(),
        "system" => nav.system,
        "plugins" => nav.plugins,
        "users" => nav.users,
        "blacklist" => nav.blacklist,
        "admin_groups" | "autogroups" => nav.groups,
        "permissions" => nav.permissions,
        "permissions_audit" => nav.permissions_audit,
        "states" => nav.states,
        "discord" => nav.discord,
        "compliance" => nav.compliance,
        "corpstats" => nav.corpstats,
        "audit" => nav.audit,
        "setup" => nav.setup,
        _ => false,
    };
    crate::menu::BUILTINS
        .iter()
        .filter(|b| may(b.key))
        .map(|b| {
            let badge = (b.key == "group_management")
                .then_some(group_management)
                .flatten();
            crate::menu::builtin_item(b, badge)
        })
        .chain(
            plugin_nav
                .iter()
                .map(|l| crate::menu::plugin_item(&l.label, &l.href)),
        )
        .collect()
}

pub struct PluginNavLink {
    pub label: String,
    pub href: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AdminNav {
    pub groups: bool,
    pub permissions: bool,
    pub states: bool,
    pub discord: bool,
    pub system: bool,
    pub plugins: bool,
    pub audit: bool,
    pub setup: bool,
    /// Officers: who isn't compliant, and Corp Stats.
    pub compliance: bool,
    /// Corporation Stats, for any of its views.
    pub corpstats: bool,
    pub permissions_audit: bool,
    pub users: bool,
    pub blacklist: bool,
    /// Not an admin page: fleet pings, for FCs.
    pub pings: bool,
}

impl AdminNav {
    pub fn any(&self) -> bool {
        self.groups
            || self.permissions
            || self.states
            || self.discord
            || self.system
            || self.plugins
            || self.audit
            || self.setup
            || self.compliance
            || self.corpstats
            || self.permissions_audit
            || self.users
            || self.blacklist
    }
}

/// `GET /`: send visitors where they belong.
pub async fn home(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Redirect, PageError> {
    if session.is_some() {
        return Ok(Redirect::to("/dashboard"));
    }
    if !accounts::owner_exists(&state.db).await? {
        return Ok(Redirect::to("/setup"));
    }
    Ok(Redirect::to("/login"))
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    setup_complete: bool,
}

/// `GET /login`
pub async fn login(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    if session.is_some() {
        return Ok(Redirect::to("/dashboard").into_response());
    }
    let setup_complete = accounts::owner_exists(&state.db).await?;
    Ok(render(StatusCode::OK, &LoginPage { setup_complete }))
}

pub struct CharacterRow {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    /// SSO revoked the character's token; logging in with it again fixes it.
    pub needs_login: bool,
    /// `registered` or `missing` when the state requires scopes; empty
    /// otherwise. Filled on the profile page only.
    pub status: &'static str,
    /// The scopes its token carries, and what uses each.
    pub scopes: Vec<ScopeLine>,
}

pub struct ScopeLine {
    pub scope: String,
    pub description: String,
    /// "Member requirement, Moon Tracker", or "Not used".
    pub used_by: String,
}

/// Fills in each character's scopes and whether it meets the state's
/// requirements (F16: the profile shows what was granted and why).
async fn annotate(
    state: &AppState,
    account: tether_db::accounts::AccountId,
    rows: &mut [CharacterRow],
) -> Result<(), AppError> {
    let registration = crate::compliance::registration(&state.db, account).await?;
    let plugins = tether_db::compliance::plugin_scopes(&state.db).await?;
    let target = registration.target.as_ref().map(|t| t.name.clone());
    for row in rows.iter_mut() {
        let Some(status) = registration.characters.iter().find(|c| c.id == row.id) else {
            continue;
        };
        if !registration.required.is_empty() {
            row.status = if status.problem.is_none() {
                "registered"
            } else {
                "missing"
            };
        }
        row.scopes = status
            .scopes
            .iter()
            .map(|scope| {
                let mut users: Vec<String> = Vec::new();
                if registration.required.contains(scope)
                    && let Some(target) = &target
                {
                    users.push(format!("{target} requirement"));
                }
                users.extend(
                    plugins
                        .iter()
                        .filter(|p| p.scopes.contains(scope))
                        .map(|p| p.name.clone()),
                );
                if scope == tether_core::scopes::CORP_MEMBERSHIP {
                    users.push("Corporation Stats".to_owned());
                }
                ScopeLine {
                    scope: scope.clone(),
                    description: tether_core::scopes::describe(scope).to_owned(),
                    used_by: if users.is_empty() {
                        "Not used".to_owned()
                    } else {
                        users.join(", ")
                    },
                }
            })
            .collect();
    }
    Ok(())
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfilePage {
    shell: Shell,
    state_style: &'static str,
    state_name: String,
    is_owner: bool,
    characters: Vec<CharacterRow>,
    groups: Vec<String>,
    permissions: Vec<String>,
    plugin_access: Vec<plugin_access::PluginAccess>,
    corp_sources: Vec<compliance::OwnSource>,
    /// `admin.system` holders get the admin panels (loaded after the page).
    system_panel: bool,
    widgets: Vec<DashboardWidget>,
    error: Option<String>,
}

/// A plugin's Dashboard widget, loaded after the page.
pub struct DashboardWidget {
    pub title: String,
    /// The fragment's address.
    pub url: String,
}

#[derive(Template)]
#[template(path = "profile_characters.html")]
struct CharactersFragment {
    characters: Vec<CharacterRow>,
    error: Option<String>,
}

pub(crate) struct Loaded {
    pub(crate) shell: Shell,
    state: AccessState,
    is_owner: bool,
    characters: Vec<CharacterRow>,
}

pub(crate) async fn load(
    state: &AppState,
    session: &CurrentSession,
    active: &'static str,
) -> Result<Loaded, PageError> {
    let account = accounts::get(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let access = state_db::account_state(&state.db, session.account)
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let token_states = tether_db::tokens::states_for_account(&state.db, session.account).await?;
    let perms = permissions::effective(&state.db, session.account).await?;
    let nav = AdminNav {
        groups: perms.contains(tether_core::permissions::ADMIN_GROUPS),
        permissions: perms.contains(tether_core::permissions::ADMIN_PERMISSIONS),
        states: perms.contains(tether_core::permissions::ADMIN_STATES),
        discord: perms.contains(tether_core::permissions::ADMIN_DISCORD),
        system: perms.contains(tether_core::permissions::ADMIN_SYSTEM),
        plugins: perms.contains(tether_core::permissions::ADMIN_PLUGINS),
        audit: perms.contains(tether_core::permissions::ADMIN_AUDIT),
        compliance: perms.contains(tether_core::permissions::COMPLIANCE_VIEW),
        corpstats: [
            tether_core::permissions::COMPLIANCE_VIEW,
            tether_core::permissions::CORPSTATS_CORP,
            tether_core::permissions::CORPSTATS_ALLIANCE,
            tether_core::permissions::CORPSTATS_STATE,
        ]
        .iter()
        .any(|p| perms.contains(*p)),
        users: perms.contains(tether_core::permissions::ADMIN_USERS),
        blacklist: perms.contains(tether_core::permissions::BLACKLIST_VIEW),
        permissions_audit: perms.contains(tether_core::permissions::PERMISSIONS_AUDIT),
        pings: perms.contains(tether_core::permissions::FLEET_PING),
        setup: account.is_owner,
    };
    let managed = crate::groups::managed_by(&state.db, session.account).await?;
    let group_management = if managed.is_empty() {
        None
    } else {
        Some(tether_db::groups::pending_count(&state.db, &managed).await?)
    };
    let plugin_nav = state
        .plugins
        .navigation()
        .into_iter()
        .filter(|item| {
            let needed = item
                .permission
                .as_deref()
                .unwrap_or(tether_core::permissions::ADMIN_PLUGINS);
            perms.contains(needed)
        })
        .map(|item| PluginNavLink {
            label: item.label,
            href: item.href,
        })
        .collect::<Vec<_>>();
    let menu =
        crate::menu::sidebar(&state.db, menu_items(&nav, group_management, &plugin_nav)).await?;
    let characters = account
        .characters
        .iter()
        .map(|c| CharacterRow {
            id: c.id,
            name: c.name.clone(),
            is_main: account.main.as_ref().is_some_and(|m| m.id == c.id),
            needs_login: token_states.get(&c.id) == Some(&tether_db::tokens::TokenState::Revoked),
            status: "",
            scopes: Vec::new(),
        })
        .collect();
    let not_compliant =
        match tether_db::compliance::not_compliant_state(&state.db, session.account).await? {
            Some(id) => state_db::get(&state.db, id).await?.map(|s| s.name),
            None => None,
        };
    Ok(Loaded {
        shell: Shell {
            user: ShellUser {
                name: account
                    .main
                    .as_ref()
                    .map_or_else(|| "No main character".to_owned(), |m| m.name.clone()),
                character_id: account.main.as_ref().map_or(0, |m| m.id),
                state_name: access.name.clone(),
                is_owner: account.is_owner,
            },
            active,
            nav,
            plugin_nav,
            menu,
            active_href: String::new(),
            not_compliant,
            no_main: account.main.is_none(),
            group_management,
            unread: tether_db::notifications::unread(&state.db, session.account).await?,
            admin_rail: crate::admin_nav::rail(&nav, active),
        },
        state: access,
        is_owner: account.is_owner,
        characters,
    })
}

/// `GET /profile`: the page is the Dashboard now, as in AA.
pub async fn to_dashboard() -> Redirect {
    Redirect::permanent("/dashboard")
}

/// `GET /dashboard`: AA's Dashboard (characters, state, groups).
pub async fn profile(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    let mut loaded = load(&state, &session, "profile").await?;
    annotate(&state, session.account, &mut loaded.characters).await?;
    let groups = tether_db::groups::names_for(&state.db, session.account).await?;
    let held = permissions::effective(&state.db, session.account).await?;
    let widgets = state
        .plugins
        .widgets()
        .into_iter()
        .filter(|w| {
            held.contains(
                w.permission
                    .as_deref()
                    .unwrap_or(tether_core::permissions::ADMIN_PLUGINS),
            )
        })
        .map(|w| DashboardWidget {
            url: format!("/dashboard/widgets/{}/{}", w.plugin_id, w.index),
            title: w.title,
        })
        .collect();
    let system_panel = held.contains(tether_core::permissions::ADMIN_SYSTEM);
    let permissions = held.into_iter().collect();
    Ok(render(
        StatusCode::OK,
        &ProfilePage {
            shell: loaded.shell,
            state_style: loaded.state.style(),
            state_name: loaded.state.name,
            is_owner: loaded.is_owner,
            characters: loaded.characters,
            groups,
            permissions,
            plugin_access: plugin_access::for_profile(&state, &session).await?,
            corp_sources: compliance::own_sources(&state, session.account).await?,
            system_panel,
            widgets,
            error: None,
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct MainForm {
    character_id: i64,
}

/// `POST /profile/main`: htmx swaps in the characters card; without htmx,
/// back to the profile.
pub async fn make_main(
    State(state): State<AppState>,
    session: CurrentSession,
    headers: HeaderMap,
    Form(form): Form<MainForm>,
) -> Result<Response, PageError> {
    let changed = accounts::set_main(&state.db, session.account, form.character_id).await?;
    if changed {
        crate::states::evaluate_account(&state.db, session.account).await?;
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to("/dashboard").into_response());
    }
    let mut loaded = load(&state, &session, "profile").await?;
    annotate(&state, session.account, &mut loaded.characters).await?;
    Ok(render(
        StatusCode::OK,
        &CharactersFragment {
            characters: loaded.characters,
            error: (!changed).then(|| "That character isn't on your account.".to_owned()),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn initials_take_two_words() {
        assert_eq!(initials("Dev Owner"), "DO");
        assert_eq!(initials("chribba"), "C");
        assert_eq!(initials("The Mittani Test"), "TM");
        assert_eq!(initials(""), "");
    }
}
