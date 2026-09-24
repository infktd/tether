//! Admin pages: groups, permissions and tier rules. Plain forms that work
//! without JavaScript (post, then redirect); htmx boosts them. Every action
//! goes through `crate::admin`, the same code as the JSON API.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{
    ADMIN_GROUPS, ADMIN_PERMISSIONS, ADMIN_TIERS, CORE_PERMISSIONS, JoinPolicy,
};
use tether_core::tiers::{EntityKind, Tier};
use tether_db::audit::Actor;
use tether_db::groups::{self, GroupId};
use tether_db::permissions::{self, Grantee};
use tether_db::{accounts, tiers as tier_db};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::admin::{self, MembershipChange};
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

fn policy_label(policy: JoinPolicy) -> &'static str {
    match policy {
        JoinPolicy::Open => "Open",
        JoinPolicy::Request => "Request to join",
        JoinPolicy::Assigned => "Assigned by admins",
    }
}

pub(crate) fn tier_label(tier: &str) -> &'static str {
    match tier {
        "member" => "Member",
        "allied" => "Allied",
        _ => "Guest",
    }
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
        (ADMIN_GROUPS, "/admin/groups"),
        (ADMIN_PERMISSIONS, "/admin/permissions"),
        (ADMIN_TIERS, "/admin/tiers"),
        (tether_core::permissions::ADMIN_DISCORD, "/admin/discord"),
        (tether_core::permissions::ADMIN_AUDIT, "/admin/audit"),
    ] {
        if perms.contains(permission) {
            return Ok(Redirect::to(page));
        }
    }
    Err(AppError::forbidden().into())
}

// ---- groups ----------------------------------------------------------------

pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub policy: &'static str,
    pub policy_label: &'static str,
    pub members: i64,
    pub pending: i64,
}

#[derive(Template)]
#[template(path = "admin_groups.html")]
struct GroupsPage {
    shell: Shell,
    groups: Vec<GroupRow>,
    error: Option<String>,
    form: NewGroupForm,
}

#[derive(Debug, Default, Deserialize)]
pub struct NewGroupForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    join_policy: String,
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
        .map(|g| GroupRow {
            id: g.group.id.0,
            name: g.group.name,
            description: g.group.description,
            policy: g.group.join_policy.as_str(),
            policy_label: policy_label(g.group.join_policy),
            members: g.members,
            pending: g.pending,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupsPage {
        shell,
        groups,
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
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    groups_page(&state, shell, NewGroupForm::default(), None).await
}

/// `POST /admin/groups`
pub async fn create_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<NewGroupForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::create_group(
        &state,
        session.account,
        &form.name,
        &form.description,
        &form.join_policy,
    )
    .await
    {
        Ok(id) => Ok(Redirect::to(&format!("/admin/groups/{}", id.0)).into_response()),
        Err(err) => groups_page(&state, shell, form, Some(err)).await,
    }
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub tier: String,
    pub tier_label: &'static str,
}

pub struct RequestRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
}

#[derive(Template)]
#[template(path = "admin_group.html")]
struct GroupPage {
    shell: Shell,
    group: GroupRow,
    members: Vec<MemberRow>,
    requests: Vec<RequestRow>,
    error: Option<String>,
}

async fn group_page(
    state: &AppState,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let group = groups::summaries(&state.db)
        .await?
        .into_iter()
        .find(|g| g.group.id.0 == id)
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let members = groups::members(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|m| MemberRow {
            tier_label: tier_label(&m.tier),
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
            tier: m.tier,
        })
        .collect();
    let requests = groups::requests(&state.db, GroupId(id))
        .await?
        .into_iter()
        .map(|r| RequestRow {
            account_id: r.account_id,
            main_id: r.main_id,
            main_name: r.main_name,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = GroupPage {
        shell,
        group: GroupRow {
            id: group.group.id.0,
            name: group.group.name,
            description: group.group.description,
            policy: group.group.join_policy.as_str(),
            policy_label: policy_label(group.group.join_policy),
            members: group.members,
            pending: group.pending,
        },
        members,
        requests,
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
    let (_, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    group_page(&state, shell, id, None).await
}

/// `POST /admin/groups/{id}/delete`
pub async fn delete_group(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::delete_group(&state, session.account, id).await {
        Ok(()) => Ok(Redirect::to("/admin/groups").into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct AddMemberForm {
    character: String,
}

/// `POST /admin/groups/{id}/members`: by character name or id.
pub async fn add_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Form(form): Form<AddMemberForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    let result = match accounts::find(&state.db, &form.character).await? {
        Some(account) => {
            admin::change_membership(
                &state,
                session.account,
                id,
                account.0,
                MembershipChange::Add,
            )
            .await
        }
        None => Err(AppError::not_found(
            "No account has a character with that name.",
        )),
    };
    match result {
        Ok(()) => Ok(Redirect::to(&format!("/admin/groups/{id}")).into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

async fn membership(
    state: AppState,
    session: Option<CurrentSession>,
    id: i64,
    account_id: i64,
    change: MembershipChange,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_GROUPS, "groups").await?;
    match admin::change_membership(&state, session.account, id, account_id, change).await {
        Ok(()) => Ok(Redirect::to(&format!("/admin/groups/{id}")).into_response()),
        Err(err) => group_page(&state, shell, id, Some(err)).await,
    }
}

/// `POST /admin/groups/{id}/members/{account_id}/remove`
pub async fn remove_member(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Remove).await
}

/// `POST /admin/groups/{id}/requests/{account_id}/approve`
pub async fn approve(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Approve).await
}

/// `POST /admin/groups/{id}/requests/{account_id}/deny`
pub async fn deny(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    membership(state, session, id, account_id, MembershipChange::Deny).await
}

// ---- permissions -----------------------------------------------------------

pub struct GrantBadge {
    pub id: i64,
    pub label: String,
    /// `tier` or `group`.
    pub kind: &'static str,
}

pub struct PermissionRow {
    pub name: &'static str,
    pub description: &'static str,
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
    groups: Vec<GroupOption>,
    error: Option<String>,
}

async fn permissions_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let grants = permissions::list(&state.db).await?;
    let all_groups = groups::summaries(&state.db).await?;
    let group_name = |id: GroupId| {
        all_groups
            .iter()
            .find(|g| g.group.id == id)
            .map_or_else(|| format!("group {}", id.0), |g| g.group.name.clone())
    };
    let rows = CORE_PERMISSIONS
        .iter()
        .map(|(name, description)| PermissionRow {
            name,
            description,
            grants: grants
                .iter()
                .filter(|g| g.permission == *name)
                .map(|g| match g.grantee {
                    Grantee::Tier(t) => GrantBadge {
                        id: g.id,
                        label: tier_label(t.as_str()).to_owned(),
                        kind: "tier",
                    },
                    Grantee::Group(group) => GrantBadge {
                        id: g.id,
                        label: group_name(group),
                        kind: "group",
                    },
                })
                .collect(),
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = PermissionsPage {
        shell,
        rows,
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
    /// `tier:member` or `group:<id>`.
    grantee: String,
}

/// A `<select>` value: `tier:member` or `group:<id>`.
pub(crate) fn parse_grantee(value: &str) -> Result<Grantee, AppError> {
    match value.split_once(':') {
        Some(("tier", tier)) => admin::grantee(Some(tier), None),
        Some(("group", id)) => id
            .parse()
            .map_err(|_| AppError::bad_request("Choose a tier or a group."))
            .and_then(|id| admin::grantee(None, Some(id))),
        _ => Err(AppError::bad_request("Choose a tier or a group.")),
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

// ---- tier rules ------------------------------------------------------------

pub struct RuleRow {
    pub entity_id: i64,
    pub name: String,
    pub kind: &'static str,
    pub tier: &'static str,
    pub tier_label: &'static str,
}

#[derive(Template)]
#[template(path = "admin_tiers.html")]
struct TiersPage {
    shell: Shell,
    rules: Vec<RuleRow>,
    error: Option<String>,
}

async fn tiers_page(
    state: &AppState,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let rules = tier_db::list_rules(&state.db)
        .await?
        .into_iter()
        .map(|r| RuleRow {
            entity_id: r.entity_id,
            name: r.name,
            kind: r.kind.as_str(),
            tier: r.tier.as_str(),
            tier_label: tier_label(r.tier.as_str()),
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let page = TiersPage {
        shell,
        rules,
        error: error.map(|e| e.message().to_owned()),
    };
    Ok(render(status, &page))
}

/// `GET /admin/tiers`
pub async fn tiers(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, ADMIN_TIERS, "tiers").await?;
    tiers_page(&state, shell, None).await
}

#[derive(Debug, Deserialize)]
pub struct RuleForm {
    entity_id: i64,
    tier: String,
}

/// `POST /admin/tiers`
pub async fn set_rule(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Form(form): Form<RuleForm>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_TIERS, "tiers").await?;
    let result = match Tier::parse(&form.tier) {
        Some(tier) => admin::apply_tier_rule(
            &state,
            Actor::Account(session.account),
            form.entity_id,
            tier,
        )
        .await
        .map(|_| ()),
        None => Err(AppError::bad_request("tier must be member or allied.")),
    };
    match result {
        Ok(()) => Ok(Redirect::to("/admin/tiers").into_response()),
        Err(err) => tiers_page(&state, shell, Some(err)).await,
    }
}

/// `POST /admin/tiers/{entity_id}/remove`
pub async fn remove_rule(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(entity_id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, ADMIN_TIERS, "tiers").await?;
    match admin::remove_tier_rule(&state, session.account, entity_id).await {
        Ok(()) => Ok(Redirect::to("/admin/tiers").into_response()),
        Err(err) => tiers_page(&state, shell, Some(err)).await,
    }
}

pub struct SearchRow {
    pub id: i64,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Template)]
#[template(path = "admin_tier_search.html")]
struct SearchFragment {
    results: Vec<SearchRow>,
}

#[derive(Debug, Deserialize)]
pub struct SearchForm {
    name: String,
}

/// `POST /admin/tiers/search` (htmx): exact-name lookup via ESI.
pub async fn search(
    State(state): State<AppState>,
    session: CurrentSession,
    Form(form): Form<SearchForm>,
) -> Result<Response, PageError> {
    session.require(&state, ADMIN_TIERS).await?;
    let name = form.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request("Enter a name.").into());
    }
    let resolved = state
        .esi
        .resolve_names(&[name.to_owned()], tether_esi::Priority::Interactive)
        .await
        .map_err(admin::esi_unavailable)?;
    let mut results: Vec<SearchRow> = resolved
        .alliances
        .into_iter()
        .map(|e| SearchRow {
            id: e.id,
            name: e.name,
            kind: EntityKind::Alliance.as_str(),
        })
        .collect();
    results.extend(resolved.corporations.into_iter().map(|e| SearchRow {
        id: e.id,
        name: e.name,
        kind: EntityKind::Corporation.as_str(),
    }));
    Ok(render(StatusCode::OK, &SearchFragment { results }))
}
