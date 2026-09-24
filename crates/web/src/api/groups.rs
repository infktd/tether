//! Groups as seen by members: list, join, leave.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::json;
use tether_core::permissions::JoinPolicy;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId};

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

#[derive(Debug, Serialize)]
pub struct GroupOut {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub join_policy: &'static str,
    pub is_member: bool,
    pub has_requested: bool,
}

/// `GET /api/groups`
pub async fn list(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<GroupOut>>, AppError> {
    let groups = groups::list_for(&state.db, session.account).await?;
    Ok(Json(
        groups
            .into_iter()
            .map(|g| GroupOut {
                id: g.group.id.0,
                name: g.group.name,
                description: g.group.description,
                join_policy: g.group.join_policy.as_str(),
                is_member: g.is_member,
                has_requested: g.has_requested,
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize)]
pub struct JoinOut {
    /// `member` (joined an open group) or `requested` (awaiting approval).
    pub status: &'static str,
}

/// `POST /api/groups/{id}/join`
pub async fn join(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<JoinOut>), AppError> {
    let group = groups::get(&state.db, GroupId(id))
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))?;
    let target = format!("group:{id}");
    let mut tx = state.db.begin().await?;
    let out = match group.join_policy {
        JoinPolicy::Open => {
            if groups::add_member(&mut *tx, group.id, session.account).await? {
                audit::record(
                    &mut *tx,
                    Actor::Account(session.account),
                    "group.join",
                    Some(&target),
                    json!({}),
                )
                .await?;
            }
            (StatusCode::OK, Json(JoinOut { status: "member" }))
        }
        JoinPolicy::Request => {
            if groups::add_request(&mut *tx, group.id, session.account).await? {
                audit::record(
                    &mut *tx,
                    Actor::Account(session.account),
                    "group.request",
                    Some(&target),
                    json!({}),
                )
                .await?;
            }
            (
                StatusCode::ACCEPTED,
                Json(JoinOut {
                    status: "requested",
                }),
            )
        }
        JoinPolicy::Assigned => {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                "Members of this group are assigned by admins.",
            ));
        }
    };
    tx.commit().await?;
    Ok(out)
}

/// `POST /api/groups/{id}/leave`: leave, or withdraw a pending request.
pub async fn leave(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let group = GroupId(id);
    let target = format!("group:{id}");
    let mut tx = state.db.begin().await?;
    if groups::remove_member(&mut *tx, group, session.account).await? {
        audit::record(
            &mut *tx,
            Actor::Account(session.account),
            "group.leave",
            Some(&target),
            json!({}),
        )
        .await?;
    } else if groups::remove_request(&mut *tx, group, session.account).await? {
        audit::record(
            &mut *tx,
            Actor::Account(session.account),
            "group.request.withdraw",
            Some(&target),
            json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
