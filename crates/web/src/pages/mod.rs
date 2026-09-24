//! Server-rendered pages (askama + Basecoat + htmx). Interactive pieces are
//! htmx requests to endpoints that return HTML fragments.

pub mod admin;
pub mod assets;
pub mod discord;
pub mod headers;
pub mod pings;
pub mod setup;

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::tiers::Tier;
use tether_db::{accounts, groups, permissions, tiers as tier_db};

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

fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Member => "Member",
        Tier::Allied => "Allied",
        Tier::Guest => "Guest",
    }
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
    pub tier_label: &'static str,
    pub is_owner: bool,
}

pub struct Shell {
    pub user: ShellUser,
    pub active: &'static str,
    /// Admin links the sidebar may show.
    pub nav: AdminNav,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AdminNav {
    pub groups: bool,
    pub permissions: bool,
    pub tiers: bool,
    pub discord: bool,
    pub setup: bool,
    /// Not an admin page: fleet pings, for FCs.
    pub pings: bool,
}

impl AdminNav {
    pub fn any(&self) -> bool {
        self.groups || self.permissions || self.tiers || self.discord || self.setup
    }
}

/// `GET /`: send visitors where they belong.
pub async fn home(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Redirect, PageError> {
    if session.is_some() {
        return Ok(Redirect::to("/profile"));
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
        return Ok(Redirect::to("/profile").into_response());
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
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfilePage {
    shell: Shell,
    tier: &'static str,
    tier_label: &'static str,
    is_owner: bool,
    characters: Vec<CharacterRow>,
    groups: Vec<String>,
    permissions: Vec<String>,
    discord: Option<discord::DiscordCard>,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "profile_characters.html")]
struct CharactersFragment {
    characters: Vec<CharacterRow>,
    error: Option<String>,
}

pub(crate) struct Loaded {
    pub(crate) shell: Shell,
    tier: Tier,
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
    let tier = tier_db::account_tier(&state.db, session.account)
        .await?
        .unwrap_or(Tier::Guest);
    let token_states = tether_db::tokens::states_for_account(&state.db, session.account).await?;
    let perms = permissions::effective(&state.db, session.account).await?;
    let nav = AdminNav {
        groups: perms.contains(tether_core::permissions::ADMIN_GROUPS),
        permissions: perms.contains(tether_core::permissions::ADMIN_PERMISSIONS),
        tiers: perms.contains(tether_core::permissions::ADMIN_TIERS),
        discord: perms.contains(tether_core::permissions::ADMIN_DISCORD),
        pings: perms.contains(tether_core::permissions::FLEET_PING),
        setup: account.is_owner,
    };
    let characters = account
        .characters
        .iter()
        .map(|c| CharacterRow {
            id: c.id,
            name: c.name.clone(),
            is_main: c.id == account.main.id,
            needs_login: token_states.get(&c.id) == Some(&tether_db::tokens::TokenState::Revoked),
        })
        .collect();
    Ok(Loaded {
        shell: Shell {
            user: ShellUser {
                name: account.main.name.clone(),
                character_id: account.main.id,
                tier_label: tier_label(tier),
                is_owner: account.is_owner,
            },
            active,
            nav,
        },
        tier,
        is_owner: account.is_owner,
        characters,
    })
}

/// `GET /profile`
pub async fn profile(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let Some(session) = session else {
        return Ok(Redirect::to("/login").into_response());
    };
    let loaded = load(&state, &session, "profile").await?;
    let groups = groups::names_for(&state.db, session.account).await?;
    let permissions = permissions::effective(&state.db, session.account)
        .await?
        .into_iter()
        .collect();
    Ok(render(
        StatusCode::OK,
        &ProfilePage {
            shell: loaded.shell,
            tier: loaded.tier.as_str(),
            tier_label: tier_label(loaded.tier),
            is_owner: loaded.is_owner,
            characters: loaded.characters,
            groups,
            permissions,
            discord: discord::card(&state, session.account).await?,
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
        crate::tiers::evaluate_account(&state.db, session.account).await?;
    }
    if !is_htmx(&headers) {
        return Ok(Redirect::to("/profile").into_response());
    }
    let loaded = load(&state, &session, "profile").await?;
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
