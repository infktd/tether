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
pub mod securegroups;
pub mod setup;
pub mod states;
pub mod stay;
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

/// CCP's image server URL for a corporation's logo (64px, shown at 20px).
/// `None` for unknown and fixture ids.
pub fn corporation_logo(id: impl CharacterId) -> Option<String> {
    let id = id.id();
    (id > 0).then(|| format!("https://images.evetech.net/corporations/{id}/logo?size=64"))
}

/// As [`corporation_logo`], for an alliance.
pub fn alliance_logo(id: impl CharacterId) -> Option<String> {
    let id = id.id();
    (id > 0).then(|| format!("https://images.evetech.net/alliances/{id}/logo?size=64"))
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

/// A page shown again with a problem on it (a refused form): with htmx,
/// the problem in a toast too, since the page keeps its scroll position
/// and its alert may be out of view.
pub(crate) fn with_problem(problem: Option<String>, response: Response) -> Response {
    match problem {
        Some(message) => stay::with_toast(response, stay::Toast::problem(message)),
        None => response,
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
    let mut response = render(
        status,
        &ErrorPage {
            status: status.as_u16(),
            title: status.canonical_reason().unwrap_or("Error"),
            message,
        },
    );
    response
        .extensions_mut()
        .insert(crate::error::Problem(message.to_owned()));
    response
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
        "securegroups" => nav.securegroups,
        // AA: the audit permission, and Group Management over some group.
        "securegroups_audit" => nav.securegroups_audit && group_management.is_some(),
        "pings" => nav.pings,
        "services" => nav.services,
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
                .map(|l| crate::menu::plugin_item(&l.label, &l.href, l.section)),
        )
        .collect()
}

pub struct PluginNavLink {
    pub label: String,
    pub href: String,
    /// Its default sidebar section.
    pub section: &'static str,
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
    /// Not an admin page: Services, for those with a service to link
    /// (Discord's access permission), as AA shows it.
    pub services: bool,
    /// Not admin pages: Secure Groups and Secure Group Audit, by
    /// allianceauth-secure-groups' permissions.
    pub securegroups: bool,
    pub securegroups_audit: bool,
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
    /// EVE SSO has a client id: until then the page points to the setup
    /// wizard instead of a login that can't work.
    sso_ready: bool,
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
    let sso_ready = tether_db::settings::get_string(&state.db, tether_db::settings::SSO_CLIENT_ID)
        .await?
        .is_some();
    Ok(render(
        StatusCode::OK,
        &LoginPage {
            setup_complete,
            sso_ready,
        },
    ))
}

pub struct CharacterRow {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    /// SSO revoked the character's token; logging in with it again fixes it.
    pub needs_login: bool,
    /// It has a working token, so Change Main can pick it directly; the
    /// others need a login with them first (as AA's token list).
    pub can_be_main: bool,
    /// Its registration status, when the state asks something of
    /// characters. Filled on the Dashboard only.
    pub status: Option<StatusChip>,
    /// Its corporation and alliance (id and name), for the Dashboard's
    /// Characters table.
    pub corporation: Option<(i64, String)>,
    pub alliance: Option<(i64, String)>,
}

/// Fills in each character's registration status, corporation and
/// alliance, for the Dashboard's Characters table. What each scope is
/// for is on Token Management.
async fn annotate(
    state: &AppState,
    account: tether_db::accounts::AccountId,
    rows: &mut [CharacterRow],
) -> Result<(), AppError> {
    let registration = crate::compliance::registration(&state.db, account).await?;
    let affiliations = tether_db::plugin_esi::account_characters(&state.db, account).await?;
    let ids: Vec<i64> = affiliations
        .iter()
        .flat_map(|c| [c.corporation_id, c.alliance_id])
        .flatten()
        .collect();
    let names = tether_db::compliance::cached_names(&state.db, &ids).await?;
    let named = |id: i64, unknown: &str| {
        (
            id,
            names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| unknown.to_owned()),
        )
    };
    for row in rows.iter_mut() {
        if let Some(status) = registration.characters.iter().find(|c| c.id == row.id) {
            row.status = StatusChip::of(&registration.required, status.problem.as_ref());
        }
        if row.status.is_none() && row.needs_login {
            row.status = Some(StatusChip::ended());
        }
        if let Some(c) = affiliations.iter().find(|c| c.id == row.id) {
            row.corporation = c
                .corporation_id
                .filter(|id| *id > 0)
                .map(|id| named(id, "Unknown corporation"));
            row.alliance = c
                .alliance_id
                .filter(|id| *id > 0)
                .map(|id| named(id, "Unknown alliance"));
        }
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
    /// Member Audit's My Characters, leading the Dashboard: the pilot's
    /// character audit, with AA's own panels after it.
    lead: Option<DashboardWidget>,
    widgets: Vec<DashboardWidget>,
}

/// The app whose first widget, My Characters, leads the Dashboard when
/// it's installed and the viewer may open it.
pub(crate) const CHARACTER_AUDIT: &str = "tether.member-audit";

/// A character's registration status as a chip: "Registered", or what
/// it's missing (which links to Register Character).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusChip {
    pub label: String,
    pub problem: bool,
}

impl StatusChip {
    /// EVE access ended (the token was revoked), whatever the state asks.
    fn ended() -> Self {
        Self {
            label: "Access ended · Register".to_owned(),
            problem: true,
        }
    }

    /// `None` when the state asks nothing of characters (Guest).
    fn of(
        required: &std::collections::BTreeSet<String>,
        problem: Option<&tether_core::scopes::Problem>,
    ) -> Option<Self> {
        use tether_core::scopes::Problem;
        if required.is_empty() {
            return None;
        }
        Some(match problem {
            None => Self {
                label: "Registered".to_owned(),
                problem: false,
            },
            Some(problem) => Self {
                label: match problem {
                    Problem::NotRegistered => "Not registered · Register".to_owned(),
                    Problem::NotRegisteredFor(apps) => {
                        format!("Not registered for {} · Register", apps.join(", "))
                    }
                    Problem::Revoked => return Some(Self::ended()),
                    Problem::Missing(scopes) if scopes.len() == 1 => {
                        "Missing 1 scope · Register".to_owned()
                    }
                    Problem::Missing(scopes) => {
                        format!("Missing {} scopes · Register", scopes.len())
                    }
                },
                problem: true,
            },
        })
    }
}

/// The footers Tether adds to the Dashboard's cards of the account's own
/// characters: status, and Make main where it can (a working token, not
/// the main already).
pub(crate) async fn card_feet(
    state: &AppState,
    account: accounts::AccountId,
) -> Result<std::collections::HashMap<i64, plugin_pages::CardFoot>, AppError> {
    let registration = crate::compliance::registration(&state.db, account).await?;
    let main = accounts::get(&state.db, account)
        .await?
        .and_then(|a| a.main)
        .map(|m| m.id);
    let tokens = tether_db::tokens::states_for_account(&state.db, account).await?;
    Ok(registration
        .characters
        .iter()
        .map(|c| {
            (
                c.id,
                plugin_pages::CardFoot {
                    character_id: c.id,
                    status: StatusChip::of(&registration.required, c.problem.as_ref()).or_else(
                        || {
                            (tokens.get(&c.id) == Some(&tether_db::tokens::TokenState::Revoked))
                                .then(StatusChip::ended)
                        },
                    ),
                    make_main: main != Some(c.id)
                        && tokens.get(&c.id) == Some(&tether_db::tokens::TokenState::Valid),
                },
            )
        })
        // Nothing to say (Guest's main): no empty footer.
        .filter(|(_, foot)| foot.status.is_some() || foot.make_main)
        .collect())
}

/// A plugin's Dashboard widget, loaded after the page.
pub struct DashboardWidget {
    pub title: String,
    /// The fragment's address.
    pub url: String,
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
        blacklist: tether_core::permissions::BLACKLIST_PAGE
            .iter()
            .any(|p| perms.contains(*p)),
        permissions_audit: perms.contains(tether_core::permissions::PERMISSIONS_AUDIT),
        pings: perms.contains(tether_core::permissions::FLEETPINGS_ACCESS),
        services: perms.contains(tether_core::permissions::DISCORD_ACCESS),
        securegroups: perms.contains(tether_core::permissions::SECUREGROUPS_ACCESS),
        securegroups_audit: perms.contains(tether_core::permissions::SECUREGROUPS_AUDIT),
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
            section: item.section,
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
            can_be_main: token_states.get(&c.id) == Some(&tether_db::tokens::TokenState::Valid),
            status: None,
            corporation: None,
            alliance: None,
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

/// `GET /dashboard`: AA's Dashboard (characters, state, groups). With
/// Member Audit it's the pilot's character audit: My Characters' card grid
/// first, then AA's panels, compactly.
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
    let mut widgets: Vec<(String, usize, DashboardWidget)> = state
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
        .map(|w| {
            (
                w.plugin_id.clone(),
                w.index,
                DashboardWidget {
                    url: format!("/dashboard/widgets/{}/{}", w.plugin_id, w.index),
                    title: w.title,
                },
            )
        })
        .collect();
    // With Member Audit (and access to it), the Dashboard is the pilot's
    // character audit, as Jay asked: its My Characters widget first.
    // Without a main it can't show anything (apps see accounts through
    // their main): AA's Characters, with Make main, instead.
    let lead = widgets
        .iter()
        .position(|(plugin, index, _)| plugin == CHARACTER_AUDIT && *index == 0)
        .map(|i| widgets.remove(i).2)
        .filter(|_| !loaded.shell.no_main);
    let widgets = widgets.into_iter().map(|(_, _, w)| w).collect();
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
            lead,
            widgets,
        },
    ))
}

#[derive(Debug, Deserialize)]
pub struct MainForm {
    character_id: i64,
}

/// `POST /profile/main`: Change Main (Make main) to a character already on
/// the account (with a working token). Back to the Dashboard, which htmx
/// reloads in place (the sidebar, the no-main banner and the state follow
/// the main), with a toast; one that can't be the main says why in a
/// toast and changes nothing.
pub async fn make_main(
    State(state): State<AppState>,
    session: CurrentSession,
    headers: HeaderMap,
    Form(form): Form<MainForm>,
) -> Result<Response, PageError> {
    let outcome = crate::ownership::change_main(&state, session.account, form.character_id).await?;
    Ok(match outcome {
        crate::ownership::ChangeMain::Done { name } => {
            stay::back("/dashboard", format!("{name} is your main now."))
        }
        // Without htmx, the Dashboard as it was.
        _ if !is_htmx(&headers) => Redirect::to("/dashboard").into_response(),
        refused => stay::with_toast(
            StatusCode::NO_CONTENT.into_response(),
            stay::Toast::problem(refused.message()),
        ),
    })
}

/// `POST /profile/main/login`: Change Main by logging in with EVE SSO, as
/// Alliance Auth's (its "add new token" on the Change Main page): the
/// character joins the account as with Add Character (moving from another
/// account if need be) and becomes the main. Asks for the same scopes as
/// Add Character, so the login never narrows a grant.
pub async fn change_main_login(
    State(state): State<AppState>,
    jar: axum_extra::extract::CookieJar,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let required = crate::compliance::registration(&state.db, session.account)
        .await?
        .required;
    let scopes = crate::compliance::ask_scopes(&state.db, session.account, required).await?;
    Ok(crate::auth::start_login(
        &state,
        jar,
        "/dashboard",
        tether_db::auth::Purpose::ChangeMain,
        &scopes,
        Some(session.account),
    )
    .await?)
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
