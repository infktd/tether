//! Groups as members see them (AA's Groups page): list, join, leave,
//! retract. The rules are in `crate::groups`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;
use tether_db::groups::GroupId;

use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::groups::{self, Joined, Left};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct GroupOut {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// `open` (in or out at once), `requestable` (leaders approve) or
    /// `internal` (admins only; listed only when you're in it).
    pub kind: &'static str,
    pub is_member: bool,
    /// A pending request: `join` or `leave`.
    pub pending: Option<&'static str>,
}

fn kind(group: &tether_db::groups::Group) -> &'static str {
    if group.flags.internal {
        "internal"
    } else if group.flags.open {
        "open"
    } else {
        "requestable"
    }
}

/// `GET /api/groups`: the groups you're in or have asked about, and the
/// ones you may join.
#[utoipa::path(get, path = "/api/groups", tag = "groups", security(("session" = [])),
    responses((status = 200, body = Vec<GroupOut>)))]
pub async fn list(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Json<Vec<GroupOut>>, AppError> {
    let groups = groups::available(&state.db, session.account).await?;
    Ok(Json(
        groups
            .into_iter()
            .map(|g| GroupOut {
                id: g.group.id.0,
                kind: kind(&g.group),
                name: g.group.name,
                description: g.group.description,
                is_member: g.is_member,
                pending: g.pending.map(|leave| if leave { "leave" } else { "join" }),
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StatusOut {
    /// `member` or `left` (done at once), or `requested` (awaiting the
    /// group's leaders).
    pub status: &'static str,
}

/// `POST /api/groups/{id}/join`
#[utoipa::path(post, path = "/api/groups/{id}/join", tag = "groups", security(("session" = [])),
    params(("id" = i64, Path, description = "Group id")),
    responses((status = 200, body = StatusOut, description = "Joined an Open group"),
              (status = 202, body = StatusOut, description = "Request sent to the group's leaders"),
              (status = 403, description = "Your state can't join, or you can't request groups"),
              (status = 404, description = "No such group"),
              (status = 409, description = "Already a member, or a request is pending")))]
pub async fn join(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<StatusOut>), AppError> {
    Ok(
        match groups::join(&state.db, session.account, GroupId(id)).await? {
            Joined::Added => (StatusCode::OK, Json(StatusOut { status: "member" })),
            Joined::Requested => (
                StatusCode::ACCEPTED,
                Json(StatusOut {
                    status: "requested",
                }),
            ),
        },
    )
}

/// `POST /api/groups/{id}/leave`
#[utoipa::path(post, path = "/api/groups/{id}/leave", tag = "groups", security(("session" = [])),
    params(("id" = i64, Path, description = "Group id")),
    responses((status = 200, body = StatusOut, description = "Left"),
              (status = 202, body = StatusOut, description = "Leave request sent to the group's leaders"),
              (status = 403, description = "Only admins change this group's members"),
              (status = 404, description = "Not a member"),
              (status = 409, description = "A request is pending")))]
pub async fn leave(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<StatusOut>), AppError> {
    Ok(
        match groups::leave(&state.db, session.account, GroupId(id)).await? {
            Left::Removed => (StatusCode::OK, Json(StatusOut { status: "left" })),
            Left::Requested => (
                StatusCode::ACCEPTED,
                Json(StatusOut {
                    status: "requested",
                }),
            ),
        },
    )
}

/// `POST /api/groups/{id}/retract`: withdraw a join request.
#[utoipa::path(post, path = "/api/groups/{id}/retract", tag = "groups", security(("session" = [])),
    params(("id" = i64, Path, description = "Group id")),
    responses((status = 204, description = "Withdrawn"),
              (status = 400, description = "Leave requests can't be withdrawn"),
              (status = 404, description = "No request")))]
pub async fn retract(
    State(state): State<AppState>,
    session: CurrentSession,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    groups::retract(&state.db, session.account, GroupId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
