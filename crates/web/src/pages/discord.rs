//! The Discord admin page, and linking from the profile page.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use tether_core::permissions::ADMIN_DISCORD;
use tether_db::discord as db;
use tether_db::groups::{self, GroupId};
use tether_db::permissions::Grantee;
use tether_discord::{Discord, GuildCheck};

use super::admin::{GroupOption, guard, parse_grantee, tier_label};
use super::{PageError, Shell, error_page, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::discord::{self, CallbackQuery, SettingsInput};
use crate::error::AppError;

pub struct MappingRow {
    pub id: i64,
    pub role_name: String,
    pub grantee: String,
    /// `tier` or `group`.
    pub kind: &'static str,
}

pub struct RoleOption {
    pub id: u64,
    pub name: String,
    /// Why it can't be mapped, if it can't.
    pub unavailable: Option<&'static str>,
}

pub struct Status {
    pub bot_name: String,
    pub guild_name: String,
    pub missing_permissions: Vec<&'static str>,
}

#[derive(Template)]
#[template(path = "admin_discord.html")]
struct DiscordPage {
    shell: Shell,
    callback_url: String,
    application_id: String,
    guild_id: String,
    has_client_secret: bool,
    has_bot_token: bool,
    nickname_template: String,
    invite_url: Option<String>,
    status: Option<Status>,
    /// Discord couldn't be checked with the saved settings.
    status_error: Option<String>,
    mappings: Vec<MappingRow>,
    roles: Vec<RoleOption>,
    ping_channels: Vec<tether_db::pings::PingChannel>,
    other_channels: Vec<tether_discord::TextChannel>,
    groups: Vec<GroupOption>,
    error: Option<String>,
}

async fn page(
    state: &AppState,
    shell: Shell,
    input: Option<&SettingsInput>,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let stored = discord::stored(state).await?;
    let (application_id, guild_id) = match input {
        // Show what they typed back, never the secrets.
        Some(input) => (input.application_id.clone(), input.guild_id.clone()),
        None => (
            stored
                .application_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            stored.guild_id.map(|id| id.to_string()).unwrap_or_default(),
        ),
    };
    let invite_url = stored
        .application_id
        .zip(stored.guild_id)
        .map(|(app, guild)| Discord::bot_invite_url(app, guild));
    let (has_client_secret, has_bot_token) =
        (stored.client_secret.is_some(), stored.bot_token.is_some());
    let nickname_template =
        tether_db::settings::get_string(&state.db, tether_db::settings::DISCORD_NICKNAME_TEMPLATE)
            .await?
            .unwrap_or_default();

    let (check, status_error) = match stored.config() {
        Some(config) => match state.discord.check(&config).await {
            Ok(check) => (Some(check), None),
            Err(err) => (None, Some(discord::discord_error(err).message().to_owned())),
        },
        None => (None, None),
    };
    let all_groups = groups::summaries(&state.db).await?;
    let group_name = |id: GroupId| {
        all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone())
    };
    let mappings = db::mappings(&state.db)
        .await?
        .into_iter()
        .map(|m| {
            let (grantee, kind) = match m.grantee {
                Grantee::Tier(t) => (tier_label(t.as_str()).to_owned(), "tier"),
                Grantee::Group(g) => (group_name(g), "group"),
            };
            // Prefer Discord's current name, in case the role was renamed.
            let role_name = check
                .as_ref()
                .and_then(|c| {
                    c.roles
                        .iter()
                        .find(|r| i64::try_from(r.id) == Ok(m.role_id))
                })
                .map_or(m.role_name, |r| r.name.clone());
            MappingRow {
                id: m.id,
                role_name,
                grantee,
                kind,
            }
        })
        .collect();
    let roles = check.as_ref().map_or_else(Vec::new, role_options);
    let status = check.map(|c| Status {
        bot_name: c.bot_name,
        guild_name: c.guild_name,
        missing_permissions: c.missing_permissions,
    });
    let (ping_channels, other_channels) = if status.is_some() {
        crate::pings::channel_options(state).await?
    } else {
        match discord::config(state).await {
            Ok(config) => (
                crate::pings::channels_for(state, &config).await?,
                Vec::new(),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        }
    };
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = DiscordPage {
        shell,
        callback_url: state.site.discord_callback_url(),
        application_id,
        guild_id,
        has_client_secret,
        has_bot_token,
        nickname_template,
        invite_url,
        status,
        status_error,
        mappings,
        roles,
        ping_channels,
        other_channels,
        groups: all_groups
            .iter()
            .map(|g| GroupOption {
                id: g.group.id.0,
                name: g.group.name.clone(),
            })
            .collect(),
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(code, &page))
}

fn role_options(check: &GuildCheck) -> Vec<RoleOption> {
    check
        .roles
        .iter()
        .map(|r| RoleOption {
            id: r.id,
            name: r.name.clone(),
            unavailable: if r.administrator {
                Some("has Administrator")
            } else if r.managed {
                Some("managed by an integration")
            } else if !r.assignable {
                Some("above the bot's role")
            } else {
                None
            },
        })
        .collect()
}

/// `GET /admin/discord`
pub async fn admin(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    page(&state, shell, None, None).await
}

/// `POST /admin/discord`: save and check the bot settings.
pub async fn save_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(input): Form<SettingsInput>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    match discord::save_settings(&state, session.account, &input).await {
        Ok(_) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, Some(&input), Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct NicknameForm {
    #[serde(default)]
    template: String,
}

/// `POST /admin/discord/nickname`
pub async fn save_nickname(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NicknameForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    match discord::save_nickname_template(&state, session.account, &form.template).await {
        Ok(()) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, None, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct MappingForm {
    role_id: String,
    /// `tier:member` or `group:<id>`.
    grantee: String,
}

/// `POST /admin/discord/mappings`
pub async fn add_mapping(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<MappingForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    let result = match parse_grantee(&form.grantee) {
        Ok(grantee) => discord::add_mapping(&state, session.account, &form.role_id, grantee).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, None, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ChannelForm {
    channel_id: String,
}

/// `POST /admin/discord/channels`
pub async fn add_channel(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<ChannelForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    match crate::pings::add_channel(&state, session.account, &form.channel_id).await {
        Ok(()) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, None, Some(err)).await,
    }
}

/// `POST /admin/discord/channels/{id}/remove`
pub async fn remove_channel(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    match crate::pings::remove_channel(&state, session.account, id).await {
        Ok(()) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, None, Some(err)).await,
    }
}

/// `POST /admin/discord/mappings/{id}/remove`
pub async fn remove_mapping(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_DISCORD, "discord").await?;
    match discord::remove_mapping(&state, session.account, id).await {
        Ok(()) => Ok(Redirect::to("/admin/discord").into_response()),
        Err(err) => page(&state, shell, None, Some(err)).await,
    }
}

/// `POST /profile/discord/link`: off to Discord to approve.
pub async fn link(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let (jar, url) = discord::begin_link(&state, session.account, jar).await?;
    Ok((jar, Redirect::to(&url)).into_response())
}

/// `POST /profile/discord/unlink`
pub async fn unlink(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    discord::unlink(&state, session.account).await?;
    Ok(Redirect::to("/profile").into_response())
}

/// `GET /discord/callback`: Discord sends the member back here.
pub async fn callback(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    jar: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let account = session.map(|s| s.account);
    let (jar, result) = discord::finish_link(&state, account, jar, query).await;
    match result {
        Ok(_) => (jar, Redirect::to("/profile")).into_response(),
        Err(err) => (jar, error_page(err.status(), err.message())).into_response(),
    }
}

/// The profile page's Discord card: `None` when Discord isn't set up.
pub struct DiscordCard {
    /// The linked Discord name.
    pub linked: Option<String>,
    /// Member or Allied: may join the server.
    pub may_join: bool,
}

pub(crate) async fn card(
    state: &AppState,
    account: tether_db::accounts::AccountId,
) -> Result<Option<DiscordCard>, PageError> {
    // A broken Discord setup mustn't take the profile page down with it.
    let configured = discord::is_configured(state).await.unwrap_or(false);
    if !configured {
        return Ok(None);
    }
    let link = db::link_for(&state.db, account).await?;
    Ok(Some(DiscordCard {
        linked: link.map(|l| l.username),
        may_join: discord::may_join(state, account).await?,
    }))
}
