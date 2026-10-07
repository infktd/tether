//! Corporation Stats (AA's): each corporation's Mains, Members and
//! Unregistered, search across them, and Update Now; for the corporations
//! the viewer's permissions cover.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use tether_core::permissions::{
    COMPLIANCE_VIEW, CORPSTATS_ALLIANCE, CORPSTATS_CORP, CORPSTATS_STATE,
};
use tether_db::corpstats::{self as db, Scope};

use super::toolbar::{self, ListQuery, ToolbarView};
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
        name: c.name.unwrap_or_else(|| "Unknown corporation".to_owned()),
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
    toolbar: ToolbarView,
    /// Corporations the viewer may see, before the toolbar's search.
    any: bool,
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
    // The search finds pilots across the corporations (from 3 letters),
    // and the corporations by name.
    let list = ListQuery::new("/corpstats").param("q", &q);
    let words = list.words();
    let any = !corporations.is_empty();
    let corporations = corporations
        .into_iter()
        .map(corp_row)
        .filter(|c| toolbar::matches(&words, &[&c.name]))
        .collect();
    Ok(render(
        StatusCode::OK,
        &ListPage {
            shell: loaded.shell,
            toolbar: ToolbarView::new(&list)
                .search("Search pilots (3 letters or more) and corporations"),
            any,
            corporations,
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
    toolbar: ToolbarView,
    /// Under the toolbar's search.
    searched: bool,
    corp: CorpRow,
    /// May Update Now (AA: officers, or the owner of the token that read it).
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
    /// The toolbar's search among the tab's rows.
    #[serde(default)]
    q: String,
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
    query: &TabQuery,
    notice: Option<String>,
) -> Result<Response, PageError> {
    let corp = visible_corp(state, session, id).await?;
    let loaded = load(state, session, "corpstats").await?;
    let tab = match query.tab.trim() {
        "members" => "members",
        "unregistered" => "unregistered",
        _ => "mains",
    };
    let list = ListQuery::new(format!("/corpstats/{id}"))
        .param("tab", if tab == "mains" { "" } else { tab })
        .param("q", &query.q.chars().take(100).collect::<String>());
    let words = list.words();
    let (mains, members) = match tab {
        "mains" => {
            let mains = db::mains(&state.db, id)
                .await?
                .into_iter()
                .filter(|m| {
                    let mut text: Vec<&str> = vec![&m.main_name];
                    text.extend(m.main_corporation.as_deref());
                    text.extend(m.characters.iter().map(String::as_str));
                    toolbar::matches(&words, &text)
                })
                .collect();
            (mains, Vec::new())
        }
        _ => {
            let all = db::members(&state.db, id).await?;
            let members = all
                .into_iter()
                .filter(|m| tab == "members" || !m.registered)
                .map(|m| MemberView {
                    id: m.character_id,
                    name: m.name.unwrap_or_else(|| "Unknown character".to_owned()),
                    registered: m.registered,
                    main: m.main_name.unwrap_or_default(),
                })
                .filter(|m| toolbar::matches(&words, &[&m.name, &m.main]))
                .collect();
            (Vec::new(), members)
        }
    };
    let toolbar = ToolbarView::new(&list).search("Search pilots").views(
        &list,
        "tab",
        [
            ("mains", "Mains"),
            ("members", "Members"),
            ("unregistered", "Unregistered"),
        ],
    );
    let can_update = may_update(state, session, id).await?;
    Ok(render(
        StatusCode::OK,
        &CorpPage {
            shell: loaded.shell,
            searched: !words.is_empty(),
            toolbar,
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
    corp_page(&state, &session, id, &query, None).await
}

/// AA's rule: officers (`compliance.view`) or the owner of the token that
/// last read the corporation's member list.
async fn may_update(state: &AppState, session: &CurrentSession, id: i64) -> Result<bool, AppError> {
    Ok(
        tether_db::permissions::effective(&state.db, session.account)
            .await?
            .contains(COMPLIANCE_VIEW)
            || db::owns_source(&state.db, session.account, id).await?,
    )
}

/// `POST /corpstats/{corporation_id}/update`: AA's Update Now, for a
/// corporation the viewer may see, by officers or the owner of the token
/// that last read it; at most every minute while ESI's budget has room,
/// every 15 minutes while it's low.
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
    let gap = crate::compliance::update_gap(&state.esi);
    let queued = db::queue_update(&mut tx, id, gap).await?;
    tx.commit().await?;
    tracing::info!(
        account = session.account.0,
        corporation = id,
        queued,
        "Corp Stats update asked for"
    );
    if queued {
        Ok(super::stay::back(
            &format!("/corpstats/{id}"),
            "Updating: the member list refreshes in a minute.",
        ))
    } else {
        corp_page(
            &state,
            &session,
            id,
            &TabQuery::default(),
            Some(if gap.as_secs() <= 60 {
                "An update is already waiting, or ran a minute ago.".to_owned()
            } else {
                format!(
                    "An update is already waiting, or ran in the last {} minutes (ESI is busy).",
                    gap.as_secs() / 60
                )
            }),
        )
        .await
    }
}
