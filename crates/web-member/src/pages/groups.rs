//! The users' Groups page (AA's Available Groups, and the direct join
//! link) and Group Management (Group Requests, Group Membership and each
//! group's Audit Log). Every action goes through `crate::groups`, the same
//! code as the JSON API.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_db::accounts::AccountId;
use tether_db::groups::{self as group_db, Group, GroupId};

use super::toolbar::{self, ListQuery, ToolbarView};
use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{self, Decision, Joined, Left};

/// A group's badge: Internal, Open or Requestable (AA's labels).
pub fn label(group: &Group) -> &'static str {
    group.flags.label()
}

pub struct GroupCard {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub internal: bool,
    pub is_member: bool,
    /// `join`, `leave`, or empty.
    pub pending: &'static str,
    /// A smart group's filters (Secure Groups), as its requirements.
    pub requirements: Vec<String>,
}

fn card(a: groups::Available) -> GroupCard {
    GroupCard {
        id: a.group.id.0,
        label: label(&a.group),
        internal: a.group.flags.internal,
        name: a.group.name,
        description: a.group.description,
        is_member: a.is_member,
        pending: match a.pending {
            Some(true) => "leave",
            Some(false) => "join",
            None => "",
        },
        requirements: Vec::new(),
    }
}

/// Fills in smart groups' requirements (Internal groups unnamed).
async fn requirements(db: &tether_db::PgPool, cards: &mut [GroupCard]) -> Result<(), AppError> {
    let mut smart = Vec::new();
    for card in cards.iter_mut() {
        if tether_db::smart_groups::active(db, GroupId(card.id))
            .await?
            .is_some()
        {
            let (rules, _) = tether_db::smart_groups::rules(db, GroupId(card.id)).await?;
            smart.push((card, rules));
        }
    }
    if smart.is_empty() {
        return Ok(());
    }
    let all: Vec<_> = smart.iter().flat_map(|(_, r)| r.iter().cloned()).collect();
    let mut conn = db.acquire().await?;
    let names = crate::smart_groups::Names::load(&mut conn, &all, true).await?;
    for (card, rules) in smart {
        card.requirements = rules.iter().map(|r| names.describe(r)).collect();
    }
    Ok(())
}

/// A list's toolbar: its search, and a filter or a tab (each page says
/// which it reads).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    action: String,
}

#[derive(Template)]
#[template(path = "groups.html")]
struct GroupsPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under a search: what the lists say when it leaves nothing.
    searched: bool,
    mine: Vec<GroupCard>,
    available: Vec<GroupCard>,
    error: Option<String>,
    /// What the account's state, groups and own grants give it, at the
    /// foot of the page.
    permissions: Vec<String>,
    is_owner: bool,
}

async fn groups_page(
    state: &AppState,
    session: &CurrentSession,
    error: Option<AppError>,
    params: &ListParams,
) -> Result<Response, PageError> {
    let loaded = load(state, session, "groups").await?;
    let list = ListQuery::new("/groups").param("q", &params.q);
    let words = list.words();
    let (mut mine, mut available): (Vec<_>, Vec<_>) = groups::available(&state.db, session.account)
        .await?
        .into_iter()
        .map(card)
        .filter(|g| toolbar::matches(&words, &[&g.name, &g.description, g.label]))
        .partition(|g| g.is_member);
    requirements(&state.db, &mut mine).await?;
    requirements(&state.db, &mut available).await?;
    let permissions = tether_db::permissions::effective(&state.db, session.account)
        .await?
        .into_iter()
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &GroupsPage {
                shell: loaded.shell,
                toolbar: ToolbarView::new(&list).search("Search groups"),
                searched: !words.is_empty(),
                mine,
                available,
                error: error.map(|e| e.message().to_owned()),
                permissions,
                is_owner: loaded.is_owner,
            },
        ),
    ))
}

/// `GET /groups`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    groups_page(&state, &session, None, &params).await
}

#[derive(Template)]
#[template(path = "group_join.html")]
struct JoinPage {
    shell: Shell,
    group: GroupCard,
    error: Option<String>,
}

/// `GET /groups/{id}`: the direct join link (works for Hidden groups).
pub async fn direct(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let group = groups::direct(&state.db, session.account, GroupId(id)).await?;
    let loaded = load(&state, &session, "groups").await?;
    Ok(render(
        StatusCode::OK,
        &JoinPage {
            shell: loaded.shell,
            group: card(group),
            error: None,
        },
    ))
}

/// Back to the page as it was (its search too), saying so; or the page
/// with the problem.
async fn after(
    state: &AppState,
    session: &CurrentSession,
    result: Result<&'static str, AppError>,
) -> Result<Response, PageError> {
    match result {
        Ok(notice) => Ok(super::stay::back("/groups", notice)),
        Err(err) => groups_page(state, session, Some(err), &ListParams::default()).await,
    }
}

/// `POST /groups/{id}/join`
pub async fn join(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::join(&state.db, session.account, GroupId(id))
        .await
        .map(|joined| match joined {
            Joined::Added => "You joined the group.",
            Joined::Requested => "Request sent: the group's leaders will decide.",
        });
    after(&state, &session, result).await
}

/// `POST /groups/{id}/leave`
pub async fn leave(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::leave(&state.db, session.account, GroupId(id))
        .await
        .map(|left| match left {
            Left::Removed => "You left the group.",
            Left::Requested => "Leave request sent: the group's leaders will decide.",
        });
    after(&state, &session, result).await
}

/// `POST /groups/{id}/retract`
pub async fn retract(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let result = groups::retract(&state.db, session.account, GroupId(id))
        .await
        .map(|()| "Request withdrawn.");
    after(&state, &session, result).await
}

// ---- Group Management ------------------------------------------------------

/// Signed in and managing at least one group (else 403).
async fn manager(
    state: &AppState,
    session: Option<CurrentSession>,
) -> Result<(CurrentSession, Shell), PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let loaded = load(state, &session, "group_management").await?;
    if loaded.shell.group_management.is_none() {
        return Err(AppError::forbidden().into());
    }
    Ok((session, loaded.shell))
}

pub struct RequestRow {
    pub group_id: i64,
    pub group_name: String,
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub org: Organization,
    pub requested_at: String,
}

/// A main's corporation and alliance (AA's "organization"), for leaders
/// deciding requests.
pub struct Organization {
    pub corporation_id: i64,
    pub corporation: String,
    /// `None` outside an alliance.
    pub alliance: Option<(i64, String)>,
}

fn organization(
    corporation_id: Option<i64>,
    corporation_name: Option<String>,
    alliance_id: Option<i64>,
    alliance_name: Option<String>,
) -> Organization {
    let corporation_id = corporation_id.unwrap_or(0);
    Organization {
        // Never the id itself (DESIGN.md: no raw ids).
        corporation: corporation_name.unwrap_or_else(|| "Unknown corporation".to_owned()),
        corporation_id,
        alliance: alliance_id.map(|id| {
            (
                id,
                alliance_name.unwrap_or_else(|| "Unknown alliance".to_owned()),
            )
        }),
    }
}

#[derive(Template)]
#[template(path = "group_management.html")]
struct RequestsPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under the toolbar's search or filter.
    searched: bool,
    joins: Vec<RequestRow>,
    leaves: Vec<RequestRow>,
    error: Option<String>,
}

async fn requests_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    error: Option<AppError>,
    params: &ListParams,
) -> Result<Response, PageError> {
    let managed = groups::managed_by(&state.db, session.account).await?;
    let requests = group_db::requests_for(&state.db, &managed).await?;
    // The groups with requests waiting, as the filter's values.
    let mut waiting: Vec<(String, String)> = Vec::new();
    for r in &requests {
        if !waiting.iter().any(|(id, _)| *id == r.group_id.to_string()) {
            waiting.push((r.group_id.to_string(), r.group_name.clone()));
        }
    }
    waiting.sort_by_key(|(_, name)| name.to_lowercase());
    let group = Some(params.group.trim()).filter(|g| waiting.iter().any(|(id, _)| id == g));
    let list = ListQuery::new("/group-management")
        .param("q", &params.q)
        .param("group", group.unwrap_or(""));
    let words = list.words();
    let (leaves, joins): (Vec<_>, Vec<_>) = requests
        .into_iter()
        .filter(|r| group.is_none_or(|g| r.group_id.to_string() == g))
        .filter(|r| {
            toolbar::matches(
                &words,
                &[
                    &r.main_name,
                    &r.group_name,
                    r.corporation_name.as_deref().unwrap_or(""),
                    r.alliance_name.as_deref().unwrap_or(""),
                ],
            )
        })
        .partition(|r| r.leave);
    let toolbar = ToolbarView::new(&list)
        .search("Search pilots, corporations and groups")
        .filter(&list, "Group", "group", waiting);
    let row = |r: group_db::PendingRequest| RequestRow {
        group_id: r.group_id,
        group_name: r.group_name,
        account_id: r.account_id,
        main_id: r.main_id,
        main_name: r.main_name,
        org: organization(
            r.corporation_id,
            r.corporation_name,
            r.alliance_id,
            r.alliance_name,
        ),
        requested_at: r.requested_at.format("%Y-%m-%d %H:%M").to_string(),
    };
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &RequestsPage {
                shell,
                searched: list.href() != list.path,
                toolbar,
                joins: joins.into_iter().map(row).collect(),
                leaves: leaves.into_iter().map(row).collect(),
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /group-management`: Group Requests.
pub async fn requests(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    requests_page(&state, &session, shell, None, &params).await
}

async fn decide(
    state: AppState,
    session: Option<CurrentSession>,
    (id, account_id): (i64, i64),
    decision: Decision,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    let message = match decision {
        Decision::Accept => "Request accepted.",
        Decision::Reject => "Request rejected.",
    };
    match groups::decide(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
        decision,
    )
    .await
    {
        Ok(()) => Ok(super::stay::back("/group-management", message)),
        Err(err) => requests_page(&state, &session, shell, Some(err), &ListParams::default()).await,
    }
}

/// `POST /group-management/{id}/requests/{account_id}/accept`
pub async fn accept(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(ids): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    decide(state, session, ids, Decision::Accept).await
}

/// `POST /group-management/{id}/requests/{account_id}/reject`
pub async fn reject(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(ids): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    decide(state, session, ids, Decision::Reject).await
}

pub struct ManagedRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub label: &'static str,
    pub members: i64,
    pub pending: i64,
    /// A Hidden group's direct join link (its only way in), to copy.
    pub join_link: Option<String>,
}

#[derive(Template)]
#[template(path = "group_membership.html")]
struct MembershipPage {
    shell: Shell,
    toolbar: ToolbarView,
    groups: Vec<ManagedRow>,
}

/// Group kinds, as a filter: `(value, label)`.
const KINDS: [(&str, &str); 3] = [
    ("internal", "Internal"),
    ("open", "Open"),
    ("requestable", "Requestable"),
];

/// `GET /group-management/membership`: Group Membership.
pub async fn membership(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    let managed = groups::managed_by(&state.db, session.account).await?;
    let kind = KINDS
        .iter()
        .find(|(value, _)| *value == params.kind.trim())
        .map(|(_, label)| *label);
    let list = ListQuery::new("/group-management/membership")
        .param("q", &params.q)
        .param(
            "kind",
            if kind.is_some() {
                params.kind.trim()
            } else {
                ""
            },
        );
    let words = list.words();
    let groups = group_db::summaries(&state.db)
        .await?
        .into_iter()
        .filter(|g| managed.contains(&g.group.id))
        .filter(|g| kind.is_none_or(|k| label(&g.group) == k))
        .filter(|g| toolbar::matches(&words, &[&g.group.name, &g.group.description]))
        .map(|g| ManagedRow {
            id: g.group.id.0,
            label: label(&g.group),
            name: g.group.name,
            description: g.group.description,
            members: g.members,
            pending: g.pending,
            join_link: g
                .group
                .flags
                .hidden
                .then(|| format!("{}/groups/{}", state.site.origin(), g.group.id.0)),
        })
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search groups")
        .filter(&list, "Kind", "kind", KINDS);
    Ok(render(
        StatusCode::OK,
        &MembershipPage {
            shell,
            toolbar,
            groups,
        },
    ))
}

pub struct MemberRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub org: Organization,
    pub state: String,
    pub state_style: String,
}

#[derive(Template)]
#[template(path = "group_members.html")]
struct MembersPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Members in all, before the toolbar's search and filter.
    total: usize,
    id: i64,
    name: String,
    label: &'static str,
    restricted: bool,
    join_link: String,
    members: Vec<MemberRow>,
    error: Option<String>,
}

async fn members_page(
    state: &AppState,
    session: &CurrentSession,
    shell: Shell,
    id: i64,
    error: Option<AppError>,
    params: &ListParams,
) -> Result<Response, PageError> {
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    let all = group_db::members(&state.db, group.id).await?;
    let total = all.len();
    let mut states: Vec<String> = Vec::new();
    for m in &all {
        if !states.contains(&m.state) {
            states.push(m.state.clone());
        }
    }
    states.sort_by_key(|s| s.to_lowercase());
    let in_state = Some(params.state.trim()).filter(|s| states.iter().any(|x| x == s));
    let list = ListQuery::new(format!("/group-management/{id}"))
        .param("q", &params.q)
        .param("state", in_state.unwrap_or(""));
    let words = list.words();
    let members = all
        .into_iter()
        .filter(|m| in_state.is_none_or(|s| m.state == s))
        .filter(|m| {
            toolbar::matches(
                &words,
                &[
                    &m.main_name,
                    m.corporation_name.as_deref().unwrap_or(""),
                    m.alliance_name.as_deref().unwrap_or(""),
                ],
            )
        })
        .map(|m| MemberRow {
            org: organization(
                m.corporation_id,
                m.corporation_name,
                m.alliance_id,
                m.alliance_name,
            ),
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
            state: m.state,
            state_style: m.state_style,
        })
        .collect();
    let status = error.as_ref().map_or(StatusCode::OK, AppError::status);
    let problem = error.as_ref().map(|e| e.message().to_owned());
    Ok(super::with_problem(
        problem,
        render(
            status,
            &MembersPage {
                shell,
                toolbar: ToolbarView::new(&list)
                    .search("Search pilots and corporations")
                    .filter(
                        &list,
                        "State",
                        "state",
                        states.iter().map(|s| (s.clone(), s.clone())),
                    ),
                total,
                id,
                label: label(&group),
                restricted: group.flags.restricted,
                join_link: format!("{}/groups/{id}", state.site.origin()),
                name: group.name,
                members,
                error: error.map(|e| e.message().to_owned()),
            },
        ),
    ))
}

/// `GET /group-management/{id}`: a group's members (View Members) and its
/// direct join link.
pub async fn members(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    members_page(&state, &session, shell, id, None, &params).await
}

/// `POST /group-management/{id}/members/{account_id}/remove`
pub async fn remove(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path((id, account_id)): Path<(i64, i64)>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    match groups::kick(
        &state.db,
        session.account,
        GroupId(id),
        AccountId(account_id),
    )
    .await
    {
        Ok(()) => Ok(super::stay::back(
            &format!("/group-management/{id}"),
            "Removed from the group.",
        )),
        Err(err) => {
            members_page(
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

pub struct LogRow {
    pub at: String,
    pub requestor: String,
    pub corporation: String,
    /// Join, Leave or Removed.
    pub kind: &'static str,
    /// Accept or Reject.
    pub action: &'static str,
    pub accepted: bool,
    pub actor: String,
}

#[derive(Template)]
#[template(path = "group_audit.html")]
struct AuditPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Under the toolbar's search or filter.
    searched: bool,
    id: i64,
    name: String,
    entries: Vec<LogRow>,
}

/// `GET /group-management/{id}/audit`: the group's Audit Log.
pub async fn audit(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (session, shell) = manager(&state, session).await?;
    let group = groups::managed_group_for(&state.db, session.account, GroupId(id)).await?;
    let kinds = [("join", "Join"), ("leave", "Leave"), ("removed", "Removed")];
    let kind = kinds
        .iter()
        .find(|(value, _)| *value == params.kind.trim())
        .map(|(_, label)| *label);
    let actions = [("accept", "Accept"), ("reject", "Reject")];
    let action = actions
        .iter()
        .find(|(value, _)| *value == params.action.trim())
        .map(|(value, _)| *value);
    let list = ListQuery::new(format!("/group-management/{id}/audit"))
        .param("q", &params.q)
        .param(
            "kind",
            if kind.is_some() {
                params.kind.trim()
            } else {
                ""
            },
        )
        .param("action", action.unwrap_or(""));
    let words = list.words();
    let entries: Vec<LogRow> = group_db::audit_log(&state.db, group.id, 200)
        .await?
        .into_iter()
        .map(|e| LogRow {
            at: e.at.format("%Y-%m-%d %H:%M").to_string(),
            requestor: e.requestor_main.unwrap_or_else(|| "(no main)".to_owned()),
            corporation: e.requestor_corporation.unwrap_or_default(),
            kind: match e.request_type.as_str() {
                "join" => "Join",
                "leave" => "Leave",
                _ => "Removed",
            },
            accepted: e.action == "accept",
            action: if e.action == "accept" {
                "Accept"
            } else {
                "Reject"
            },
            actor: e.actor_name.unwrap_or_default(),
        })
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        .filter(|r| action.is_none_or(|a| r.accepted == (a == "accept")))
        .filter(|r| toolbar::matches(&words, &[&r.requestor, &r.corporation, &r.actor]))
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search pilots, corporations and leaders")
        .filter(&list, "Type", "kind", kinds)
        .filter(&list, "Action", "action", actions);
    Ok(render(
        StatusCode::OK,
        &AuditPage {
            shell,
            searched: list.href() != list.path,
            toolbar,
            id,
            name: group.name,
            entries,
        },
    ))
}
