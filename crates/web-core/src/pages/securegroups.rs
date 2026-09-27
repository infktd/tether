//! allianceauth-secure-groups' pages: `/securegroups` (Secure Groups, for
//! `securegroups.access_sec_group`): the smart groups a pilot may join,
//! each checked against them, with Join, Request and Leave; and
//! `/securegroups/audit` (Secure Group Audit, for
//! `securegroups.audit_sec_group` with Group Management over the group):
//! every member against every filter, Check now, and removing members.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use tether_core::permissions::{SECUREGROUPS_ACCESS, SECUREGROUPS_AUDIT};
use tether_db::accounts::AccountId;
use tether_db::groups::GroupId;

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{Joined, Left};
use crate::smart_groups::{AuditRow, Offered};

fn when(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

#[derive(Template)]
#[template(path = "securegroups.html")]
struct SecureGroupsPage {
    shell: Shell,
    groups: Vec<Offered>,
    error: Option<String>,
}

async fn guard(
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

async fn page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let groups = crate::smart_groups::offered(&state.db, session.account).await?;
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &SecureGroupsPage {
                shell,
                groups,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /securegroups`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_ACCESS, "securegroups").await?;
    page(&state, &session, shell, None).await
}

/// The group, if it's on the account's Secure Groups page (else not
/// found: the page doesn't reveal others).
async fn offered(state: &AppState, account: AccountId, id: i64) -> Result<GroupId, AppError> {
    let group = GroupId(id);
    if crate::smart_groups::is_offered(&state.db, account, group).await? {
        Ok(group)
    } else {
        Err(AppError::not_found("No such group."))
    }
}

/// `POST /securegroups/{id}/join`: join, or ask to (AA's
/// `group_request_add`, filters checked).
pub async fn join(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_ACCESS, "securegroups").await?;
    let result = match offered(&state, session.account, id).await {
        Ok(group) => crate::groups::join_secure(&state.db, session.account, group).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(Joined::Added) => Ok(super::stay::back("/securegroups", "Joined.")),
        Ok(Joined::Requested) => Ok(super::stay::back("/securegroups", "Request sent.")),
        Err(err) => page(&state, &session, shell, Some(err)).await,
    }
}

/// `POST /securegroups/{id}/leave` (AA's `group_request_leave`).
pub async fn leave(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_ACCESS, "securegroups").await?;
    let result = match offered(&state, session.account, id).await {
        Ok(group) => crate::groups::leave(&state.db, session.account, group).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(Left::Removed) => Ok(super::stay::back("/securegroups", "Left.")),
        Ok(Left::Requested) => Ok(super::stay::back("/securegroups", "Request sent.")),
        Err(err) => page(&state, &session, shell, Some(err)).await,
    }
}

// ---- Secure Group Audit ----------------------------------------------------

pub struct AuditListRow {
    pub id: i64,
    pub name: String,
    pub auto: bool,
    pub members: i64,
    pub pending_removal: usize,
}

#[derive(Template)]
#[template(path = "securegroups_audit.html")]
struct AuditListPage {
    shell: Shell,
    groups: Vec<AuditListRow>,
}

/// The switched-on smart groups the account manages (AA: every
/// non-Internal one with Group Management, else the ones it leads).
async fn audited(state: &AppState, account: AccountId) -> Result<Vec<GroupId>, AppError> {
    let managed = crate::groups::managed_by(&state.db, account).await?;
    let mut out = Vec::new();
    for (group, settings) in tether_db::smart_groups::all(&state.db).await? {
        if settings.enabled && managed.contains(&group) {
            out.push(group);
        }
    }
    Ok(out)
}

/// `GET /securegroups/audit`
pub async fn audit_list(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_AUDIT, "securegroups_audit").await?;
    let audited = audited(&state, session.account).await?;
    let mut groups = Vec::new();
    for summary in tether_db::groups::summaries(&state.db).await? {
        if !audited.contains(&summary.group.id) {
            continue;
        }
        let pending_removal = tether_db::smart_groups::grace(&state.db, summary.group.id)
            .await?
            .len();
        let auto = tether_db::smart_groups::settings(&state.db, summary.group.id)
            .await?
            .is_some_and(|s| s.auto_join);
        groups.push(AuditListRow {
            id: summary.group.id.0,
            name: summary.group.name,
            auto,
            members: summary.members,
            pending_removal,
        });
    }
    Ok(render(StatusCode::OK, &AuditListPage { shell, groups }))
}

#[derive(Template)]
#[template(path = "securegroups_audit_group.html")]
struct AuditPage {
    shell: Shell,
    id: i64,
    name: String,
    filters: Vec<String>,
    rows: Vec<AuditRow>,
    error: Option<String>,
}

async fn audit_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
) -> Result<Response, PageError> {
    let group = GroupId(id);
    if !audited(state, session.account).await?.contains(&group) {
        return Err(AppError::not_found("No such group.").into());
    }
    let name = tether_db::groups::get(&state.db, group)
        .await?
        .map(|g| g.name)
        .unwrap_or_default();
    let (filters, rows) =
        crate::smart_groups::audit_group(&state.db, session.account, group).await?;
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &AuditPage {
                shell,
                id,
                name,
                filters,
                rows,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /securegroups/audit/{id}`
pub async fn audit(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_AUDIT, "securegroups_audit").await?;
    audit_page(&state, &session, shell, id, None).await
}

/// `POST /securegroups/audit/{id}/check`: Check now (AA's manual
/// refresh).
pub async fn check_now(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_AUDIT, "securegroups_audit").await?;
    let result = if audited(&state, session.account)
        .await?
        .contains(&GroupId(id))
    {
        crate::smart_groups::check_now(&state.db, session.account, GroupId(id)).await
    } else {
        Err(AppError::not_found("No such group."))
    };
    match result {
        Ok(changed) => Ok(super::stay::back(
            &format!("/securegroups/audit/{id}"),
            match changed {
                0 => "Checked: nothing changed.".to_owned(),
                1 => "Checked: 1 membership changed.".to_owned(),
                n => format!("Checked: {n} memberships changed."),
            },
        )),
        Err(err) => audit_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// `POST /securegroups/audit/{id}/members/{account_id}/remove` (AA's
/// `group_membership_remove`: Group Management over the group).
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_AUDIT, "securegroups_audit").await?;
    let result = if audited(&state, session.account)
        .await?
        .contains(&GroupId(id))
    {
        crate::groups::kick(
            &state.db,
            session.account,
            GroupId(id),
            AccountId(account_id),
        )
        .await
    } else {
        Err(AppError::not_found("No such group."))
    };
    match result {
        Ok(()) => Ok(super::stay::back(
            &format!("/securegroups/audit/{id}"),
            "Removed.",
        )),
        Err(err) => audit_page(&state, &session, shell, id, Some(err)).await,
    }
}

/// For the templates.
pub fn date(at: &chrono::DateTime<chrono::Utc>) -> String {
    when(at)
}
