//! allianceauth-secure-groups' pages: `/securegroups` (Secure Groups, for
//! `securegroups.access_sec_group`): the smart groups a pilot may join,
//! each checked against them, with Join, Request and Leave; and
//! `/securegroups/audit` (Secure Group Audit, for
//! `securegroups.audit_sec_group` with Group Management over the group):
//! every member against every filter, Check now, and removing members.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_core::permissions::{SECUREGROUPS_ACCESS, SECUREGROUPS_AUDIT};
use tether_db::accounts::AccountId;
use tether_db::groups::GroupId;

use super::toolbar::{self, ListQuery, ToolbarView};
use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{Joined, Left};
use crate::smart_groups::{AuditRow, Offered};

fn when(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M").to_string()
}

/// A list's toolbar: its search and its filter.
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    /// Secure Groups: `member`, `eligible` or `blocked`. The audit:
    /// `auto` or `requests`; a group's members: `kept`, `leaving` or
    /// `blocked`.
    #[serde(default)]
    show: String,
}

/// The filter's value, if it's one of `choices`.
fn chosen<'a>(params: &ListParams, choices: &[(&'a str, &str)]) -> Option<&'a str> {
    choices
        .iter()
        .find(|(value, _)| *value == params.show.trim())
        .map(|(value, _)| *value)
}

#[derive(Template)]
#[template(path = "securegroups.html")]
struct SecureGroupsPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Groups offered, before the toolbar's search and filter.
    any: bool,
    groups: Vec<Offered>,
    error: Option<String>,
}

/// Secure Groups' filter: where the pilot stands.
const STANDING: [(&str, &str); 3] = [
    ("member", "Member"),
    ("eligible", "Can join"),
    ("blocked", "Can't join yet"),
];

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
    params: &ListParams,
) -> Result<Response, PageError> {
    let offered = crate::smart_groups::offered(&state.db, session.account).await?;
    let show = chosen(params, &STANDING);
    let list = ListQuery::new("/securegroups")
        .param("q", &params.q)
        .param("show", show.unwrap_or(""));
    let words = list.words();
    let any = !offered.is_empty();
    let groups = offered
        .into_iter()
        .filter(|o| match show {
            Some("member") => o.member,
            Some("eligible") => !o.member && o.passes,
            Some(_) => !o.member && !o.passes,
            None => true,
        })
        .filter(|o| toolbar::matches(&words, &[&o.group.name, &o.group.description]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search groups")
        .filter(&list, "Standing", "show", STANDING);
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &SecureGroupsPage {
                shell,
                toolbar,
                any,
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
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_ACCESS, "securegroups").await?;
    page(&state, &session, shell, None, &params).await
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
        Err(err) => page(&state, &session, shell, Some(err), &ListParams::default()).await,
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
        Err(err) => page(&state, &session, shell, Some(err), &ListParams::default()).await,
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
    toolbar: ToolbarView,
    /// Groups to audit, before the toolbar's search and filter.
    any: bool,
    groups: Vec<AuditListRow>,
}

/// The audit's filter: how members join.
const JOINING: [(&str, &str); 2] = [("auto", "Auto"), ("requests", "On request")];

/// A group's members' filter: their standing.
const KEPT: [(&str, &str); 3] = [
    ("kept", "Kept"),
    ("leaving", "Leaving"),
    ("blocked", "Blocked"),
];

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
    Query(params): Query<ListParams>,
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
    let show = chosen(&params, &JOINING);
    let list = ListQuery::new("/securegroups/audit")
        .param("q", &params.q)
        .param("show", show.unwrap_or(""));
    let words = list.words();
    let any = !groups.is_empty();
    let groups = groups
        .into_iter()
        .filter(|g| show.is_none_or(|s| g.auto == (s == "auto")))
        .filter(|g| toolbar::matches(&words, &[&g.name]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search groups")
        .filter(&list, "Joining", "show", JOINING);
    Ok(render(
        StatusCode::OK,
        &AuditListPage {
            shell,
            toolbar,
            any,
            groups,
        },
    ))
}

#[derive(Template)]
#[template(path = "securegroups_audit_group.html")]
struct AuditPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Members in all, before the toolbar's search and filter.
    total: usize,
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
    params: &ListParams,
) -> Result<Response, PageError> {
    let group = GroupId(id);
    if !audited(state, session.account).await?.contains(&group) {
        return Err(AppError::not_found("No such group.").into());
    }
    let name = tether_db::groups::get(&state.db, group)
        .await?
        .map(|g| g.name)
        .unwrap_or_default();
    let (filters, all) =
        crate::smart_groups::audit_group(&state.db, session.account, group).await?;
    let show = chosen(params, &KEPT);
    let list = ListQuery::new(format!("/securegroups/audit/{id}"))
        .param("q", &params.q)
        .param("show", show.unwrap_or(""));
    let words = list.words();
    let total = all.len();
    let rows = all
        .into_iter()
        .filter(|r| match show {
            Some("blocked") => r.blocked.is_some(),
            Some("leaving") => r.blocked.is_none() && r.grace_until.is_some(),
            Some(_) => r.blocked.is_none() && r.grace_until.is_none(),
            None => true,
        })
        .filter(|r| toolbar::matches(&words, &[&r.main_name]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search pilots")
        .filter(&list, "Standing", "show", KEPT);
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &AuditPage {
                shell,
                toolbar,
                total,
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
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = guard(&state, session, SECUREGROUPS_AUDIT, "securegroups_audit").await?;
    audit_page(&state, &session, shell, id, None, &params).await
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
        Err(err) => {
            audit_page(
                &state,
                &session,
                shell,
                id,
                Some(err),
                &ListParams::default(),
            )
            .await
        }
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
        Err(err) => {
            audit_page(
                &state,
                &session,
                shell,
                id,
                Some(err),
                &ListParams::default(),
            )
            .await
        }
    }
}

/// For the templates.
pub fn date(at: &chrono::DateTime<chrono::Utc>) -> String {
    when(at)
}
