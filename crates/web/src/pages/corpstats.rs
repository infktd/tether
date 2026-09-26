//! Corporation Stats (AA's): each corporation's Mains, Members and
//! Unregistered, search across them, and Update Now; for the corporations
//! the viewer's permissions cover.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use tether_core::permissions::{
    COMPLIANCE_VIEW, CORPSTATS_ALLIANCE, CORPSTATS_CORP, CORPSTATS_STATE,
};
use tether_db::corpstats::{self as db, Scope};

use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;

/// The viewer's Corporation Stats scope (403 if none).
async fn scope(state: &AppState, session: &CurrentSession) -> Result<Scope, AppError> {
    let perms = tether_db::permissions::effective(&state.db, session.account).await?;
    let scope = Scope {
        all: perms.contains(COMPLIANCE_VIEW),
        corporation: perms.contains(CORPSTATS_CORP),
        alliance: perms.contains(CORPSTATS_ALLIANCE),
        state: perms.contains(CORPSTATS_STATE),
    };
    if !scope.any() {
        return Err(AppError::forbidden());
    }
    Ok(scope)
}

fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M EVE").to_string()
}

pub struct CorpRow {
    pub id: i64,
    pub name: String,
    pub members: i32,
    pub mains: i64,
    pub registered: i64,
    pub unregistered: i64,
    pub fetched: String,
}

fn corp_row(c: db::Corporation) -> CorpRow {
    CorpRow {
        id: c.corporation_id,
        name: c
            .name
            .unwrap_or_else(|| format!("Corporation {}", c.corporation_id)),
        members: c.members,
        mains: c.mains,
        registered: c.registered,
        unregistered: (i64::from(c.members) - c.registered).max(0),
        fetched: when(c.fetched_at),
    }
}

#[derive(Template)]
#[template(path = "corpstats.html")]
struct ListPage {
    shell: Shell,
    corporations: Vec<CorpRow>,
    query: String,
    found: Vec<db::Found>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// `GET /corpstats`: the corporations, and a search across them.
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(query): Query<SearchQuery>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    let scope = scope(&state, &session).await?;
    let loaded = load(&state, &session, "corpstats").await?;
    let corporations = db::visible(&state.db, session.account, scope).await?;
    let q = query.q.trim().chars().take(100).collect::<String>();
    let found = if q.chars().count() >= 3 {
        let ids: Vec<i64> = corporations.iter().map(|c| c.corporation_id).collect();
        db::search(&state.db, &ids, &q).await?
    } else {
        Vec::new()
    };
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell: loaded.shell,
            corporations: corporations.into_iter().map(corp_row).collect(),
            query: q,
            found,
        },
    ))
}

pub struct MemberView {
    pub id: i64,
    pub name: String,
    pub registered: bool,
    pub main: String,
}

#[derive(Template)]
#[template(path = "corpstats_corp.html")]
struct CorpPage {
    shell: Shell,
    corp: CorpRow,
    /// May Update Now (AA: officers, or the source's owner).
    can_update: bool,
    tab: &'static str,
    mains: Vec<db::MainRow>,
    members: Vec<MemberView>,
    notice: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TabQuery {
    #[serde(default)]
    tab: String,
}

async fn visible_corp(
    state: &AppState,
    session: &CurrentSession,
    id: i64,
) -> Result<db::Corporation, AppError> {
    let scope = scope(state, session).await?;
    db::visible(&state.db, session.account, scope)
        .await?
        .into_iter()
        .find(|c| c.corporation_id == id)
        .ok_or_else(|| AppError::not_found("No such corporation, or not one you may see."))
}

async fn corp_page(
    state: &AppState,
    session: &CurrentSession,
    id: i64,
    tab: &str,
    notice: Option<String>,
) -> Result<Response, PageError> {
    let corp = visible_corp(state, session, id).await?;
    let loaded = load(state, session, "corpstats").await?;
    let tab = match tab {
        "members" => "members",
        "unregistered" => "unregistered",
        _ => "mains",
    };
    let (mains, members) = match tab {
        "mains" => (db::mains(&state.db, id).await?, Vec::new()),
        _ => {
            let all = db::members(&state.db, id).await?;
            let members = all
                .into_iter()
                .filter(|m| tab == "members" || !m.registered)
                .map(|m| MemberView {
                    id: m.character_id,
                    name: m
                        .name
                        .unwrap_or_else(|| format!("Character {}", m.character_id)),
                    registered: m.registered,
                    main: m.main_name.unwrap_or_default(),
                })
                .collect();
            (Vec::new(), members)
        }
    };
    let can_update = may_update(state, session, id).await?;
    Ok(render(
        StatusCode::OK,
        &CorpPage {
            shell: loaded.shell,
            corp: corp_row(corp),
            can_update,
            tab,
            mains,
            members,
            notice,
        },
    ))
}

/// `GET /corpstats/{corporation_id}?tab=mains|members|unregistered`
pub async fn show(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
    Query(query): Query<TabQuery>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    corp_page(&state, &session, id, &query.tab, None).await
}

/// AA's rule: officers (`compliance.view`) or the owner of one of the
/// corporation's sources.
async fn may_update(state: &AppState, session: &CurrentSession, id: i64) -> Result<bool, AppError> {
    Ok(
        tether_db::permissions::effective(&state.db, session.account)
            .await?
            .contains(COMPLIANCE_VIEW)
            || db::owns_source(&state.db, session.account, id).await?,
    )
}

/// `POST /corpstats/{corporation_id}/update`: AA's Update Now, for a
/// corporation the viewer may see, by officers or its source's owner; at
/// most every 15 minutes.
pub async fn update(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Path(id): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    visible_corp(&state, &session, id).await?;
    if !may_update(&state, &session, id).await? {
        return Err(AppError::forbidden().into());
    }
    let mut tx = state.db.begin().await?;
    let queued = db::queue_update(&mut tx, id).await?;
    tx.commit().await?;
    tracing::info!(
        account = session.account.0,
        corporation = id,
        queued,
        "Corp Stats update asked for"
    );
    if queued {
        Ok(Redirect::to(&format!("/corpstats/{id}")).into_response())
    } else {
        corp_page(
            &state,
            &session,
            id,
            "",
            Some("An update is already waiting, or ran in the last 15 minutes.".to_owned()),
        )
        .await
    }
}
