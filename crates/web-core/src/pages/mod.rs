//! Server-rendered pages (askama + Basecoat + htmx). Interactive pieces are
//! htmx requests to endpoints that return HTML fragments.

pub mod assets;
pub mod headers;
pub mod plugin_access;
pub mod stay;

use askama::Template;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
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
pub fn render(status: StatusCode, template: &impl Template) -> Response {
    match template.render() {
        Ok(html) => (status, Html(html)).into_response(),
        Err(err) => AppError::internal(err).into_response(),
    }
}

/// A page shown again with a problem on it (a refused form): with htmx,
/// the problem in a toast too, since the page keeps its scroll position
/// and its alert may be out of view.
pub fn with_problem(problem: Option<String>, response: Response) -> Response {
    match problem {
        Some(message) => stay::with_toast(response, stay::Toast::problem(message)),
        None => response,
    }
}

/// Percent-encodes a query component.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `1,240,000,000`.
pub fn grouped(n: i64) -> String {
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

/// `1 warning`, `2 warnings`, `1,240 pilots`: a count and its noun.
pub fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// A schedule's interval in words: `every hour`, `every 5 minutes`,
/// `every 2 days`.
pub fn every(secs: i64) -> String {
    let (n, one, many) = match secs {
        s if s > 0 && s % 86_400 == 0 => (s / 86_400, "day", "days"),
        s if s > 0 && s % 3_600 == 0 => (s / 3_600, "hour", "hours"),
        s if s > 0 && s % 60 == 0 => (s / 60, "minute", "minutes"),
        s => (s, "second", "seconds"),
    };
    if n == 1 {
        format!("every {one}")
    } else {
        format!("every {} {many}", grouped(n))
    }
}

/// How long ago, in its largest whole unit: `just now`, `4m ago`, `3h ago`,
/// `2d ago`.
pub fn ago(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => "just now".to_owned(),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// How long until, the same way: `in 4m`, `in 3h`, `in 2d`; `due` once
/// it's passed.
pub fn until(seconds: i64) -> String {
    match seconds {
        s if s <= 0 => "due".to_owned(),
        s if s < 60 => format!("in {s}s"),
        s if s < 3_600 => format!("in {}m", s / 60),
        s if s < 86_400 => format!("in {}h", s / 3_600),
        s => format!("in {}d", s / 86_400),
    }
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
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

pub fn error_page(status: StatusCode, message: &str) -> Response {
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
    /// Acting as a character other than the main (Change character).
    pub acting: bool,
    /// The account's characters to act as, the current one marked.
    pub switch: Vec<ShellCharacter>,
}

pub struct ShellCharacter {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    pub current: bool,
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
    /// The Dashboard is the character audit's My characters (DESIGN.md,
    /// Dashboard): its app is installed and the account, with a main, may
    /// open that page. The app's own sidebar link then goes, and its pages
    /// mark the Dashboard.
    pub character_audit: bool,
    /// Pending requests, when the account may open Group Management.
    pub group_management: Option<i64>,
    /// Unread notifications, for the top bar.
    pub unread: i64,
    /// On Administration's pages: the page's group, as the views bar under
    /// its header (`templates/admin_views.html`), and the group's name.
    pub admin_views: Vec<crate::admin_nav::Link>,
    pub admin_group: &'static str,
    /// Sidebar sections this browser folded.
    pub folded: Vec<String>,
    /// The site's own name, for the browser tab (`crate::site_name`).
    pub site_name: Option<String>,
    /// This build's version, at the sidebar's foot.
    pub version: &'static str,
    /// For holders of `admin.system`: a newer release is out.
    pub update_available: bool,
    /// For app admins: every app's data sources, working and not.
    pub data_sources: Option<SourceHealth>,
}

/// Data sources working and not, for the sidebar's foot.
pub struct SourceHealth {
    pub working: i64,
    pub broken: i64,
}

impl SourceHealth {
    /// The segmented bar: a cell per data source (at most 24), broken
    /// ones last.
    pub fn cells(&self) -> Vec<bool> {
        let total = (self.working + self.broken).clamp(0, 24);
        let broken = self.broken.clamp(0, total);
        (0..total).map(|i| i < total - broken).collect()
    }
}

impl Shell {
    /// The site name's initials, for the command bar's chip.
    pub fn site_initials(&self) -> Option<String> {
        self.site_name.as_deref().map(initials)
    }
}

/// The sidebar items an account may see: built-in pages by its
/// permissions, then apps' pages.
pub fn menu_items(
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
                .map(|l| crate::menu::plugin_item(&l.label, &l.href, l.section, l.icon)),
        )
        .collect()
}

pub struct PluginNavLink {
    pub label: String,
    pub href: String,
    /// Its default sidebar section.
    pub section: &'static str,
    /// The app's icon (its manifest's, else a generic one).
    pub icon: &'static str,
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
pub async fn annotate(
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

/// The app whose main page, My characters, is the Dashboard for whoever
/// may open it (DESIGN.md, Dashboard).
pub const CHARACTER_AUDIT: &str = "tether.member-audit";

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

/// Tether's own footer under the card of one of the viewer's characters
/// on the Dashboard (DESIGN.md, Dashboard): its registration status, and
/// Make main when it has working EVE access and isn't the main. The app
/// never sees it.
#[derive(Clone, Debug)]
pub struct CardFoot {
    pub character_id: i64,
    pub status: Option<StatusChip>,
    pub make_main: bool,
}

/// The footers Tether adds to the Dashboard's cards of the account's own
/// characters: status, and Make main where it can (a working token, not
/// the main already).
pub async fn card_feet(
    state: &AppState,
    account: accounts::AccountId,
) -> Result<std::collections::HashMap<i64, CardFoot>, AppError> {
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
                CardFoot {
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

/// Groups named under the Dashboard's title; Groups lists them all.
pub const DASHBOARD_GROUPS: usize = 4;

pub struct Loaded {
    pub shell: Shell,
    pub state: AccessState,
    pub is_owner: bool,
    pub characters: Vec<CharacterRow>,
}

pub async fn load(
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
    // Only the Member Audit that comes with Tether: a package under its id
    // installed from elsewhere never stands in for the Dashboard.
    let character_audit = account.main.is_some()
        && state
            .plugins
            .running(CHARACTER_AUDIT)
            .filter(|running| running.origin == tether_db::plugins::Origin::Bundled)
            .is_some_and(|running| {
                crate::plugins::may_open(
                    &running.manifest.page_access(""),
                    access.is_blacklist(),
                    |p| perms.contains(p),
                )
            });
    let audit_href = crate::plugins::page_href(CHARACTER_AUDIT, "");
    let plugin_nav = state
        .plugins
        .navigation()
        .into_iter()
        .filter(|item| {
            crate::plugins::may_open(&item.access, access.is_blacklist(), |p| perms.contains(p))
        })
        // Its link would only lead to the Dashboard again.
        .filter(|item| !(character_audit && item.href == audit_href))
        .map(|item| PluginNavLink {
            label: item.label,
            href: item.href,
            section: item.section,
            icon: item.icon,
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
    // Change character: one of the account's own, not the main.
    let main_id = account.main.as_ref().map(|m| m.id);
    let acting = session
        .acting
        .filter(|id| Some(*id) != main_id)
        .and_then(|id| account.characters.iter().find(|c| c.id == id));
    let (admin_group, admin_views) = crate::admin_nav::views(&nav, active);
    Ok(Loaded {
        shell: Shell {
            user: ShellUser {
                name: acting
                    .or(account.main.as_ref())
                    .map_or_else(|| "No main character".to_owned(), |m| m.name.clone()),
                character_id: acting.or(account.main.as_ref()).map_or(0, |m| m.id),
                state_name: access.name.clone(),
                is_owner: account.is_owner,
                acting: acting.is_some(),
                switch: if account.characters.len() > 1 {
                    account
                        .characters
                        .iter()
                        .map(|c| ShellCharacter {
                            id: c.id,
                            name: c.name.clone(),
                            is_main: main_id == Some(c.id),
                            current: acting.map_or(main_id == Some(c.id), |a| a.id == c.id),
                        })
                        .collect()
                } else {
                    Vec::new()
                },
            },
            active,
            nav,
            plugin_nav,
            menu,
            active_href: String::new(),
            not_compliant,
            no_main: account.main.is_none(),
            character_audit,
            group_management,
            unread: tether_db::notifications::unread(&state.db, session.account).await?,
            admin_views,
            admin_group,
            folded: session.folded.clone(),
            site_name: crate::site_name::get(&state.db).await?,
            version: crate::updates::CURRENT,
            update_available: nav.system && crate::updates::status(&state.db).await?.newer,
            data_sources: if nav.plugins {
                let (working, broken) =
                    tether_db::plugin_esi::data_source_health(&state.db).await?;
                (working + broken > 0).then_some(SourceHealth { working, broken })
            } else {
                None
            },
        },
        state: access,
        is_owner: account.is_owner,
        characters,
    })
}

#[cfg(test)]
mod tests {
    use super::{ago, every, initials, plural, until};

    #[test]
    fn counts_take_their_noun() {
        assert_eq!(plural(1, "warning", "warnings"), "1 warning");
        assert_eq!(plural(0, "warning", "warnings"), "0 warnings");
        assert_eq!(plural(12_400, "pilot", "pilots"), "12,400 pilots");
    }

    #[test]
    fn schedules_read_in_plain_words() {
        assert_eq!(every(3_600), "every hour");
        assert_eq!(every(6 * 3_600), "every 6 hours");
        assert_eq!(every(300), "every 5 minutes");
        assert_eq!(every(60), "every minute");
        assert_eq!(every(86_400), "every day");
        assert_eq!(every(2 * 86_400), "every 2 days");
        assert_eq!(every(45), "every 45 seconds");
    }

    #[test]
    fn times_read_in_their_largest_unit() {
        assert_eq!(ago(-5), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(4 * 60 + 59), "4m ago");
        assert_eq!(ago(3 * 3_600), "3h ago");
        assert_eq!(ago(2 * 86_400 + 5), "2d ago");
        assert_eq!(until(0), "due");
        assert_eq!(until(-30), "due");
        assert_eq!(until(42), "in 42s");
        assert_eq!(until(4 * 60), "in 4m");
        assert_eq!(until(16 * 3_600 + 59), "in 16h");
        assert_eq!(until(3 * 86_400), "in 3d");
    }

    #[test]
    fn initials_take_two_words() {
        assert_eq!(initials("Dev Owner"), "DO");
        assert_eq!(initials("chribba"), "C");
        assert_eq!(initials("The Mittani Test"), "TM");
        assert_eq!(initials(""), "");
    }
}
