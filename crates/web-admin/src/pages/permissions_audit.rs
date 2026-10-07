//! Permissions Audit (AA's permissions tool): every permission, how many
//! states, groups, users and accounts hold it, and who, through what.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_core::permissions::PERMISSIONS_AUDIT;
use tether_db::permissions_audit::{self as db, Holder};

use super::admin::guard;
use super::toolbar::{self, ListQuery, ToolbarView};
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

pub struct Row {
    pub name: String,
    pub description: String,
    pub states: i64,
    pub groups: i64,
    pub users: i64,
    pub accounts: i64,
}

#[derive(Template)]
#[template(path = "permissions_audit.html")]
struct ListPage {
    shell: Shell,
    toolbar: ToolbarView,
    rows: Vec<Row>,
}

/// The toolbar's search and filter: whether a permission is held
/// (`yes`, `no`), or who a holder has it through (`state`, `group`,
/// `user`, `superuser`).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    held: String,
    #[serde(default)]
    via: String,
}

const HELD: [(&str, &str); 2] = [("yes", "Held"), ("no", "Held by nobody")];
const VIA: [(&str, &str); 4] = [
    ("state", "State"),
    ("group", "Group"),
    ("user", "User"),
    ("superuser", "Superuser"),
];

fn one_of<'a>(value: &str, choices: &[(&'a str, &str)]) -> Option<&'a str> {
    choices
        .iter()
        .find(|(v, _)| *v == value.trim())
        .map(|(v, _)| *v)
}

/// `GET /admin/permissions/audit`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, PERMISSIONS_AUDIT, "permissions_audit").await?;
    let held = one_of(&params.held, &HELD);
    let list = ListQuery::new("/admin/permissions/audit")
        .param("q", &params.q)
        .param("held", held.unwrap_or(""));
    let words = list.words();
    let mut rows = Vec::new();
    for (name, description) in tether_db::permissions::available(&state.db).await? {
        if !toolbar::matches(&words, &[&name, &description]) {
            continue;
        }
        let counts = db::counts(&state.db, &name).await?;
        let any = counts.states + counts.groups + counts.users + counts.accounts > 0;
        if held.is_some_and(|h| (h == "yes") != any) {
            continue;
        }
        rows.push(Row {
            name,
            description,
            states: counts.states,
            groups: counts.groups,
            users: counts.users,
            accounts: counts.accounts,
        });
    }
    let toolbar = ToolbarView::new(&list)
        .search("A permission, or what it allows")
        .filter(&list, "Held", "held", HELD);
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell,
            toolbar,
            rows,
        },
    ))
}

#[derive(Template)]
#[template(path = "permissions_audit_one.html")]
struct OnePage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Holders in all, before the toolbar's search and filter.
    total: usize,
    name: String,
    description: String,
    holders: Vec<Holder>,
}

/// `GET /admin/permissions/audit/{permission}`
pub async fn show(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(permission): Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, PERMISSIONS_AUDIT, "permissions_audit").await?;
    let (name, description) = tether_db::permissions::available(&state.db)
        .await?
        .into_iter()
        .find(|(name, _)| *name == permission)
        .ok_or_else(|| AppError::not_found("No such permission."))?;
    let all = db::holders(&state.db, &name).await?;
    let total = all.len();
    let via = one_of(&params.via, &VIA);
    let list = ListQuery::new(format!("/admin/permissions/audit/{}", super::encode(&name)))
        .param("q", &params.q)
        .param("via", via.unwrap_or(""));
    let words = list.words();
    let holders = all
        .into_iter()
        .filter(|h| match via {
            Some("state") => h.via_state,
            Some("group") => !h.via_groups.is_empty(),
            Some("user") => h.via_user,
            Some(_) => h.owner,
            None => true,
        })
        .filter(|h| {
            let mut text: Vec<&str> = vec![&h.main_name, &h.state];
            text.extend(h.via_groups.iter().map(String::as_str));
            toolbar::matches(&words, &text)
        })
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("Search pilots, states and groups")
        .filter(&list, "Through", "via", VIA);
    Ok(render(
        StatusCode::OK,
        &OnePage {
            shell,
            toolbar,
            total,
            name,
            description,
            holders,
        },
    ))
}
