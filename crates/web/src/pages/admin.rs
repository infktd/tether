//! Admin pages: groups and permissions (states are in `states`). Plain forms that work
//! without JavaScript (post, then redirect); htmx boosts them. Every action
//! goes through `crate::admin`, the same code as the JSON API.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::groups::Flags;
use tether_core::permissions::{ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_STATES};
use tether_db::accounts::{self, AccountId};
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::admin;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// Signed in (else the login page) and holding `permission` (else 403).
pub(crate) async fn guard(
    state: &AppState,
    session: Option<CurrentSession>,
    permission: &str,
    active: &'static str,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    session.require(state, permission).await?;
    let loaded = load(state, &session, active).await?;
    Ok((session, loaded.shell))
}

/// A state in a `<select>`.
pub struct StateOption {
    pub id: i64,
    pub name: String,
}

pub(crate) async fn state_options(state: &AppState) -> Result<Vec<StateOption>, AppError> {
    Ok(tether_db::states::list(&state.db)
        .await?
        .into_iter()
        .map(|s| StateOption {
            id: s.id.0,
            name: s.name,
        })
        .collect())
}

/// A state's name for display, from a list loaded once.
pub(crate) fn state_name(states: &[StateOption], id: tether_core::states::StateId) -> String {
    states
        .iter()
        .find(|s| s.id == id.0)
        .map_or_else(|| format!("state {}", id.0), |s| s.name.clone())
}

/// `GET /admin`: the first admin page this account may see.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Redirect, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let perms = permissions::effective(&state.db, session.account).await?;
    for (permission, page) in [
        (tether_core::permissions::ADMIN_SYSTEM, "/admin/system"),
        (tether_core::permissions::ADMIN_PLUGINS, "/admin/plugins"),
        (tether_core::permissions::ADMIN_USERS, "/admin/users"),
        (ADMIN_GROUPS, "/admin/groups"),
        (ADMIN_PERMISSIONS, "/admin/permissions"),
        (ADMIN_STATES, "/admin/states"),
        (tether_core::permissions::ADMIN_DISCORD, "/admin/discord"),
        (tether_core::permissions::ADMIN_AUDIT, "/admin/audit"),
        (
            tether_core::permissions::PERMISSIONS_AUDIT,
            "/admin/permissions/audit",
        ),
        (tether_core::permissions::COMPLIANCE_VIEW, "/compliance"),
    ] {
        if perms.contains(permission) {
            return Ok(Redirect::to(page));
        }
    }
    Err(AppError::forbidden().into())
}

// ---- groups ----------------------------------------------------------------

/// A form's fields as pairs, so checkboxes that repeat a name (the allowed
/// states) all arrive.
type Fields = Vec<(String, String)>;

fn field<'a>(fields: &'a Fields, name: &str) -> &'a str {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn checked(fields: &Fields, name: &str) -> bool {
    fields.iter().any(|(k, _)| k == name)
}

fn flags_from(fields: &Fields) -> Flags {
    Flags {
        internal: checked(fields, "internal"),
        hidden: checked(fields, "hidden"),
        open: checked(fields, "open"),
        public: checked(fields, "public"),
        restricted: checked(fields, "restricted"),
    }
}

pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub hidden: bool,
    pub public: bool,
    pub restricted: bool,
    pub compliance: bool,
    pub members: i64,
    pub pending: i64,
}

fn group_row(g: groups::GroupSummary) -> GroupRow {
    GroupRow {
        id: g.group.id.0,
        label: g.group.flags.label(),
        hidden: g.group.flags.hidden,
        public: g.group.flags.public,
        restricted: g.group.flags.restricted,
        compliance: g.group.compliance,
        name: g.group.name,
        description: g.group.description,
        members: g.members,
        pending: g.pending,
    }
}

pub struct ReservedRow {
    pub name: String,
    pub reason: String,
}

/// What the new-group form had, to show it again after an error.
pub struct NewGroupForm {
    pub name: String,
    pub description: String,
    pub flags: Flags,
}

impl Default for NewGroupForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            // AA's defaults.
            flags: Flags {
                internal: true,
                hidden: true,
                ..Flags::default()
            },
        }
    }
}

#[derive(Template)]
#[template(path = "admin_groups.html")]
struct GroupsPage {
    shell: Shell,
    groups: Vec<GroupRow>,
    reserved: Vec<ReservedRow>,
    options: crate::groups::Options,
    error: Option<String>,
    form: NewGroupForm,
}

async fn groups_page(
    state: &AppState,
    shell: Shell,
    form: NewGroupForm,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let groups = groups::summaries(&state.db)
        .await?
        .into_iter()
        .map(group_row)
        .collect();
    let reserved = groups::reserved(&state.db)
        .await?
        .into_iter()
        .map(|r| ReservedRow {
            name: r.name,
            reason: r.reason,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupsPage {
        shell,
        groups,
        reserved,
        options: crate::groups::options(&state.db).await?,
        error: error.map(|e| e.message().to_owned()),
        form,
    };
    Ok(render(status, &page))
}

/// `GET /admin/groups`
pub async fn groups(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    groups_page(&state, shell, NewGroupForm::default(), None).await
}

/// `POST /admin/groups`
pub async fn create_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let form = NewGroupForm {
        name: field(&fields, "name").to_owned(),
        description: field(&fields, "description").to_owned(),
        flags: flags_from(&fields),
    };
    match crate::groups::create(
        &state.db,
        session.account,
        &form.name,
        &form.description,
        form.flags,
    )
    .await
    {
        Ok(id) => Ok(Redirect::to(&format!("/admin/groups/{}", id.0)).into_response()),
        Err(err) => groups_page(&state, shell, form, Some(err)).await,
    }
}

/// `POST /admin/groups/settings`: auto-leave and request notifications.
pub async fn group_options(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let options = crate::groups::Options {
        auto_leave: checked(&fields, "auto_leave"),
        notify_requests: checked(&fields, "notify_requests"),
    };
    match crate::groups::set_options(&state.db, session.account, options).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ReserveForm {
    name: String,
    #[serde(default)]
    reason: String,
}

/// `POST /admin/groups/reserved`
pub async fn reserve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<ReserveForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::reserve(&state.db, session.account, &form.name, &form.reason).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

/// `POST /admin/groups/reserved/remove`
pub async fn unreserve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<ReserveForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::unreserve(&state.db, session.account, &form.name).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => groups_page(&state, shell, NewGroupForm::default(), Some(err)).await,
    }
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub state: String,
    pub state_style: String,
}

pub struct LeaderRow {
    pub account_id: i64,
    pub main_id: i64,
    pub name: String,
}

pub struct StateChoice {
    pub id: i64,
    pub name: String,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "admin_group.html")]
struct GroupPage {
    shell: Shell,
    group: GroupRow,
    flags: Flags,
    states: Vec<StateChoice>,
    leaders: Vec<LeaderRow>,
    leader_groups: Vec<GroupOption>,
    other_groups: Vec<GroupOption>,
    members: Vec<MemberRow>,
    /// Secure Groups: its settings and filters, if it's a smart group.
    smart: Option<SmartView>,
    error: Option<String>,
}

pub struct SmartView {
    pub auto_join: bool,
    pub grace_days: i32,
    pub notify: bool,
    /// `(id, what it asks)`.
    pub filters: Vec<(i64, String)>,
}

async fn group_page(
    state: &AppState,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let all = groups::summaries(&state.db).await?;
    let found = all
        .iter()
        .find(|g| g.group.id.0 == id)
        .cloned()
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let allowed = groups::allowed_states(&state.db, GroupId(id)).await?;
    let states = tether_db::states::list(&state.db)
        .await?
        .into_iter()
        .map(|s| StateChoice {
            checked: allowed.contains(&s.id),
            id: s.id.0,
            name: s.name,
        })
        .collect();
    let leading = groups::leader_groups(&state.db, GroupId(id)).await?;
    let (leader_groups, other_groups): (Vec<_>, Vec<_>) = all
        .iter()
        .filter(|g| g.group.id.0 != id)
        .map(|g| GroupOption {
            id: g.group.id.0,
            name: g.group.name.clone(),
        })
        .partition(|g| leading.contains(&GroupId(g.id)));
    let leaders = groups::leaders(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|(account, main_id, name)| LeaderRow {
            account_id: account.0,
            main_id,
            name,
        })
        .collect();
    let members = groups::members(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|m| MemberRow {
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
            state: m.state,
            state_style: m.state_style,
        })
        .collect();
    let smart = match tether_db::smart_groups::settings(&state.db, GroupId(id)).await? {
        Some(s) => {
            let (rules, broken) = tether_db::smart_groups::rules(&state.db, GroupId(id)).await?;
            let mut conn = state.db.acquire().await?;
            let names = crate::smart_groups::Names::load(&mut conn, &rules, false).await?;
            let mut filters: Vec<(i64, String)> =
                rules.iter().map(|r| (r.id, names.describe(r))).collect();
            // Kept out of sweeps until deleted: shown so they can be.
            filters.extend(
                broken
                    .into_iter()
                    .map(|id| (id, "a filter that no longer reads (delete it)".to_owned())),
            );
            Some(SmartView {
                auto_join: s.auto_join,
                grace_days: s.grace_days,
                notify: s.notify,
                filters,
            })
        }
        None => None,
    };
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupPage {
        shell,
        smart,
        flags: found.group.flags,
        group: group_row(found),
        states,
        leaders,
        leader_groups,
        other_groups,
        members,
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

/// `GET /admin/groups/{id}`
pub async fn group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    group_page(&state, shell, id, None).await
}

/// Runs a change to one group and shows its page again.
async fn on_group(
    state: &AppState,
    shell: Shell,
    id: i64,
    result: Result<(), AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(()) => Ok(Redirect::to(&format!("/admin/groups/{id}")).into_response()),
        Err(err) => group_page(state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/groups/{id}/smart`: make it a smart group (Secure
/// Groups), change its settings, or (`smart` unticked) make it ordinary.
pub async fn smart_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match field(&fields, "grace_days").trim().parse::<i32>() {
        _ if !checked(&fields, "smart") => {
            crate::smart_groups::set_settings(&state.db, session.account, GroupId(id), None).await
        }
        Ok(grace_days) => {
            crate::smart_groups::set_settings(
                &state.db,
                session.account,
                GroupId(id),
                Some(tether_db::smart_groups::Settings {
                    auto_join: checked(&fields, "auto_join"),
                    grace_days,
                    notify: checked(&fields, "notify"),
                }),
            )
            .await
        }
        Err(_) => Err(AppError::bad_request("A grace period is 0 to 60 days.")),
    };
    on_group(&state, shell, id, result).await
}

/// Ids from a comma-separated list of corporation and alliance names or
/// ids, each resolved through ESI (never trusted as typed).
async fn entities(state: &AppState, text: &str) -> Result<Vec<i64>, AppError> {
    let parts: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() || parts.len() > 20 {
        return Err(AppError::bad_request(
            "Name 1 to 20 corporations or alliances, separated by commas.",
        ));
    }
    let mut ids = Vec::new();
    for part in parts {
        let is_org = |kind: Option<tether_core::states::EntityKind>| {
            matches!(
                kind,
                Some(
                    tether_core::states::EntityKind::Corporation
                        | tether_core::states::EntityKind::Alliance
                )
            )
        };
        if let Ok(id) = part.parse::<i64>() {
            let named = tether_esi::names::resolve(
                &state.db,
                &state.esi,
                &[id],
                tether_esi::Priority::Interactive,
            )
            .await
            .map_err(crate::admin::names_unavailable)?;
            match named.get(&id) {
                Some(n) if is_org(n.kind()) => ids.push(id),
                _ => {
                    return Err(AppError::bad_request(format!(
                        "{id} isn't a corporation or alliance EVE knows."
                    )));
                }
            }
        } else {
            let resolved = state
                .esi
                .resolve_names(&[part.to_owned()], tether_esi::Priority::Interactive)
                .await
                .map_err(crate::admin::esi_unavailable)?;
            let found: Vec<i64> = resolved
                .corporations
                .iter()
                .chain(resolved.alliances.iter())
                .map(|e| e.id)
                .collect();
            if found.is_empty() {
                return Err(AppError::bad_request(format!(
                    "No corporation or alliance is named exactly {part}."
                )));
            }
            // Keep the names for describing the filter later.
            tether_esi::names::resolve(
                &state.db,
                &state.esi,
                &found,
                tether_esi::Priority::Interactive,
            )
            .await
            .map_err(crate::admin::names_unavailable)?;
            ids.extend(found);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// `POST /admin/groups/{id}/smart/filters`: one filter; `kind` says which,
/// `reversed` flips it.
pub async fn smart_filter(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    use tether_core::smart::Filter;
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let ids = |name: &str| -> Result<Vec<i64>, AppError> {
        let list: Vec<i64> = fields
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.parse())
            .collect::<Result<_, _>>()
            .map_err(|_| AppError::bad_request("Choose from the list."))?;
        if list.is_empty() {
            return Err(AppError::bad_request("Choose at least one."));
        }
        Ok(list)
    };
    let filter = match field(&fields, "kind") {
        "state" => ids("states").map(|states| Filter::State { states }),
        "main_affiliation" => entities(&state, field(&fields, "entities"))
            .await
            .map(|entities| Filter::MainAffiliation { entities }),
        "any_affiliation" => entities(&state, field(&fields, "entities"))
            .await
            .map(|entities| Filter::AnyAffiliation { entities }),
        "character_age" => field(&fields, "days")
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|d| (1..=36_500).contains(d))
            .map(|days| Filter::CharacterAge { days })
            .ok_or_else(|| AppError::bad_request("Give an age in days.")),
        "groups" => ids("groups").map(|groups| Filter::Groups {
            groups,
            all: field(&fields, "match") == "all",
        }),
        "compliant" => Ok(Filter::Compliant {}),
        _ => Err(AppError::bad_request("Choose a filter.")),
    };
    let result = match filter {
        Ok(filter) => {
            crate::smart_groups::add_filter(
                &state.db,
                session.account,
                GroupId(id),
                filter,
                checked(&fields, "reversed"),
            )
            .await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/smart/filters/{filter}/delete`
pub async fn smart_filter_delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, filter)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result =
        crate::smart_groups::delete_filter(&state.db, session.account, GroupId(id), filter).await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/settings`
pub async fn group_settings(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(fields): Form<Fields>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let states = fields
        .iter()
        .filter(|(k, _)| k == "states")
        .map(|(_, v)| v.parse().map(tether_core::states::StateId))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| AppError::bad_request("Choose states from the list."));
    let result = match states {
        Ok(states) => {
            crate::groups::update(
                &state.db,
                session.account,
                GroupId(id),
                crate::groups::Settings {
                    description: field(&fields, "description").to_owned(),
                    flags: flags_from(&fields),
                    compliance: checked(&fields, "compliance"),
                    states,
                },
            )
            .await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/delete`
pub async fn delete_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    match crate::groups::delete(&state.db, session.account, GroupId(id)).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct CharacterForm {
    character: String,
}

async fn find_account(state: &AppState, character: &str) -> Result<AccountId, AppError> {
    accounts::find(&state.db, character)
        .await?
        .ok_or_else(|| AppError::not_found("No account has a character with that name."))
}

/// `POST /admin/groups/{id}/members`: by character name or id.
pub async fn add_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<CharacterForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match find_account(&state, &form.character).await {
        Ok(account) => {
            crate::groups::add_member(&state.db, session.account, GroupId(id), account).await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/members/{account_id}/remove`
pub async fn remove_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::remove_member(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
    )
    .await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leaders`: by character name or id.
pub async fn add_leader(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<CharacterForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = match find_account(&state, &form.character).await {
        Ok(account) => {
            crate::groups::set_leader(&state.db, session.account, GroupId(id), account, true).await
        }
        Err(err) => Err(err),
    };
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leaders/{account_id}/remove`
pub async fn remove_leader(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
        false,
    )
    .await;
    on_group(&state, shell, id, result).await
}

#[derive(Debug, Deserialize)]
pub struct LeaderGroupForm {
    group_id: i64,
}

/// `POST /admin/groups/{id}/leader-groups`
pub async fn add_leader_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<LeaderGroupForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader_group(
        &state.db,
        session.account,
        GroupId(id),
        GroupId(form.group_id),
        true,
    )
    .await;
    on_group(&state, shell, id, result).await
}

/// `POST /admin/groups/{id}/leader-groups/{leader_group_id}/remove`
pub async fn remove_leader_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, leader_group_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "admin_groups").await?;
    let result = crate::groups::set_leader_group(
        &state.db,
        session.account,
        GroupId(id),
        GroupId(leader_group_id),
        false,
    )
    .await;
    on_group(&state, shell, id, result).await
}

// ---- permissions -----------------------------------------------------------

pub struct GrantBadge {
    pub id: i64,
    pub label: String,
    /// `state` or `group`.
    pub kind: &'static str,
}

pub struct PermissionRow {
    pub name: String,
    pub description: String,
    pub grants: Vec<GrantBadge>,
}

pub struct GroupOption {
    pub id: i64,
    pub name: String,
}

#[derive(Template)]
#[template(path = "admin_permissions.html")]
struct PermissionsPage {
    shell: Shell,
    rows: Vec<PermissionRow>,
    states: Vec<StateOption>,
    groups: Vec<GroupOption>,
    error: Option<String>,
}

async fn permissions_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let grants = permissions::list(&state.db).await?;
    let states = state_options(state).await?;
    let all_groups = groups::summaries(&state.db).await?;
    let group_name = |id: GroupId| {
        all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone())
    };
    let rows = permissions::available(&state.db)
        .await?
        .into_iter()
        .map(|(name, description)| PermissionRow {
            grants: grants
                .iter()
                .filter(|g| g.permission == name)
                .map(|g| match g.grantee {
                    Grantee::State(id) => GrantBadge {
                        id: g.id,
                        label: state_name(&states, id),
                        kind: "state",
                    },
                    Grantee::Group(group) => GrantBadge {
                        id: g.id,
                        label: group_name(group),
                        kind: "group",
                    },
                })
                .collect(),
            name,
            description,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = PermissionsPage {
        shell,
        rows,
        states,
        groups: all_groups
            .iter()
            .map(|g| GroupOption {
                id: g.group.id.0,
                name: g.group.name.clone(),
            })
            .collect(),
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

/// `GET /admin/permissions`
pub async fn permissions(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    permissions_page(&state, shell, None).await
}

#[derive(Debug, Deserialize)]
pub struct GrantForm {
    permission: String,
    /// `state:<id>` or `group:<id>`.
    grantee: String,
}

/// A `<select>` value: `state:<id>` or `group:<id>`.
pub(crate) fn parse_grantee(value: &str) -> Result<Grantee, AppError> {
    let choose = || AppError::bad_request("Choose a state or a group.");
    match value.split_once(':') {
        Some(("state", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(Some(id), None)),
        Some(("group", id)) => id
            .parse()
            .map_err(|_| choose())
            .and_then(|id| admin::grantee(None, Some(id))),
        _ => Err(choose()),
    }
}

/// `POST /admin/permissions/grant`
pub async fn grant(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<GrantForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    let result = match parse_grantee(&form.grantee) {
        Ok(grantee) => admin::grant(&state, session.account, &form.permission, grantee)
            .await
            .map(|_| ()),
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => Ok(Redirect::to("/admin/permissions").into_response()),
        Err(err) => permissions_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/permissions/{grant_id}/revoke`
pub async fn revoke(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(grant_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_PERMISSIONS, "permissions").await?;
    match admin::revoke(&state, session.account, grant_id).await {
        Ok(()) => Ok(Redirect::to("/admin/permissions").into_response()),
        Err(err) => permissions_page(&state, shell, Some(err)).await,
    }
}
