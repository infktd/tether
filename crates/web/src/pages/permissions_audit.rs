//! Permissions Audit (AA's permissions tool): every permission, how many
//! states, groups and accounts hold it, and who, through what.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use tether_core::permissions::PERMISSIONS_AUDIT;
use tether_db::permissions_audit::{self as db, Holder};

use super::admin::guard;
use super::{PageError, Shell, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

pub struct Row {
    pub name: String,
    pub description: String,
    pub states: i64,
    pub groups: i64,
    pub accounts: i64,
}

#[derive(Template)]
#[template(path = "permissions_audit.html")]
struct ListPage {
    shell: Shell,
    rows: Vec<Row>,
}

/// `GET /admin/permissions/audit`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, PERMISSIONS_AUDIT, "permissions_audit").await?;
    let mut rows = Vec::new();
    for (name, description) in tether_db::permissions::available(&state.db).await? {
        let counts = db::counts(&state.db, &name).await?;
        rows.push(Row {
            name,
            description,
            states: counts.states,
            groups: counts.groups,
            accounts: counts.accounts,
        });
    }
    Ok(render(StatusCode::OK, &ListPage { shell, rows }))
}

#[derive(Template)]
#[template(path = "permissions_audit_one.html")]
struct OnePage {
    shell: Shell,
    name: String,
    description: String,
    holders: Vec<Holder>,
}

/// `GET /admin/permissions/audit/{permission}`
pub async fn show(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(permission): Path<String>,
) -> Result<Response, PageError> {
    let (_, shell) = guard(&state, session, PERMISSIONS_AUDIT, "permissions_audit").await?;
    let (name, description) = tether_db::permissions::available(&state.db)
        .await?
        .into_iter()
        .find(|(name, _)| *name == permission)
        .ok_or_else(|| AppError::not_found("No such permission."))?;
    let holders = db::holders(&state.db, &name).await?;
    Ok(render(
        StatusCode::OK,
        &OnePage {
            shell,
            name,
            description,
            holders,
        },
    ))
}
