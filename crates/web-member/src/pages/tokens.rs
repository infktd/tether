//! Token Management: the signed-in account's tokens, and what each scope
//! they carry is for (the Dashboard only says whether a character is
//! registered).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;

use super::stay::{Toast, notice, with_toast};
use super::toolbar::{self, ListQuery, ToolbarView, current_query};
use super::{PageError, Shell, load, render};
use crate::AppState;
use crate::auth::CurrentSession;
use crate::error::AppError;
use crate::tokens::{self, Refreshed};

pub struct ScopeRow {
    pub scope: String,
    pub description: String,
    /// "Member requirement, Moon Mining", or "Not used".
    pub used_by: String,
}

pub struct TokenRow {
    pub character_id: i64,
    pub name: String,
    pub is_main: bool,
    pub scopes: Vec<ScopeRow>,
    /// The scopes in one line: "29 scopes · all required granted".
    pub summary: String,
    /// It falls short of what the state requires.
    pub missing: bool,
    pub revoked: bool,
    pub deleted: bool,
    pub created: String,
    pub refreshed: String,
}

#[derive(Template)]
#[template(path = "tokens.html")]
struct TokensPage {
    shell: Shell,
    toolbar: ToolbarView,
    /// Tokens in all, before the toolbar's search and filter.
    total: usize,
    rows: Vec<TokenRow>,
    /// The account's characters that apps read corporation data through.
    owners: Vec<super::plugin_access::OwnSource>,
    notice: Option<String>,
    error: Option<String>,
}

/// "29 scopes · all required granted", "29 scopes · 3 required missing".
fn summary(count: usize, required: usize, missing: usize) -> String {
    let scopes = if count == 1 {
        "1 scope".to_owned()
    } else {
        format!("{count} scopes")
    };
    match (required, missing) {
        (0, _) => scopes,
        (_, 0) => format!("{scopes} · all required granted"),
        (_, n) => format!("{scopes} · {n} required missing"),
    }
}

/// The toolbar: a character or a scope, and how the tokens are
/// (`working`, `missing`: short of the state's scopes, `broken`).
#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    #[serde(default)]
    status: String,
}

const STATUSES: [(&str, &str); 3] = [
    ("working", "Working"),
    ("missing", "Missing scopes"),
    ("broken", "Not working"),
];

const PATH: &str = "/tokens";

async fn tokens_page(
    state: &AppState,
    session: &CurrentSession,
    notice: Option<String>,
    error: Option<AppError>,
    params: &ListParams,
) -> Result<Response, PageError> {
    let loaded = load(state, session, "tokens").await?;
    let registration = crate::compliance::registration(&state.db, session.account).await?;
    // Apps read a character's scopes only while it's registered for them,
    // for pilots holding one of their permissions (F16).
    let mut plugins = Vec::new();
    for p in tether_db::compliance::plugin_scopes(&state.db).await? {
        if !p.scopes.is_empty()
            && tether_db::compliance::holds_app_permission(&state.db, session.account, &p.id)
                .await?
        {
            let registered =
                tether_db::compliance::registered_for_app(&state.db, session.account, &p.id)
                    .await?;
            plugins.push((p, registered));
        }
    }
    let target = registration.target.as_ref().map(|t| t.name.clone());
    // What uses a scope: the state's requirement, apps, Corporation Stats.
    let used_by = |character: i64, scope: &str| {
        let mut users: Vec<String> = Vec::new();
        if registration.required.contains(scope)
            && let Some(target) = &target
        {
            users.push(format!("{target} requirement"));
        }
        users.extend(
            plugins
                .iter()
                .filter(|(p, registered)| {
                    registered.contains(&character) && p.scopes.iter().any(|s| s.as_str() == scope)
                })
                .map(|(p, _)| p.name.clone()),
        );
        if scope == tether_core::scopes::CORP_MEMBERSHIP {
            users.push("Corporation Stats".to_owned());
        }
        if users.is_empty() {
            "Not used".to_owned()
        } else {
            users.join(", ")
        }
    };
    let all: Vec<TokenRow> = tokens::list(&state.db, session.account)
        .await?
        .into_iter()
        .map(|t| {
            let revoked = t.revoked;
            let missing = if revoked {
                registration.required.len()
            } else {
                registration
                    .required
                    .iter()
                    .filter(|r| !t.scopes.contains(r))
                    .count()
            };
            TokenRow {
                character_id: t.character_id,
                name: t.character_name,
                is_main: t.is_main,
                summary: summary(t.scopes.len(), registration.required.len(), missing),
                missing: missing > 0,
                scopes: t
                    .scopes
                    .iter()
                    .map(|s| ScopeRow {
                        description: tether_core::scopes::describe(s).to_owned(),
                        used_by: used_by(t.character_id, s),
                        scope: s.clone(),
                    })
                    .collect(),
                revoked,
                deleted: t.revoked_reason.as_deref() == Some("deleted"),
                created: t.created_at.format("%Y-%m-%d").to_string(),
                refreshed: t.last_refreshed_at.map_or_else(
                    || "never".to_owned(),
                    |at| at.format("%Y-%m-%d %H:%M").to_string(),
                ),
            }
        })
        .collect();
    let status = STATUSES
        .iter()
        .find(|(value, _)| *value == params.status.trim())
        .map(|(value, _)| *value);
    let list = ListQuery::new(PATH)
        .param("q", &params.q)
        .param("status", status.unwrap_or(""));
    let words = list.words();
    let total = all.len();
    let rows = all
        .into_iter()
        .filter(|t| match status {
            Some("working") => !t.revoked && !t.missing,
            Some("missing") => !t.revoked && t.missing,
            Some(_) => t.revoked,
            None => true,
        })
        .filter(|t| {
            let mut text: Vec<&str> = vec![&t.name];
            text.extend(t.scopes.iter().map(|s| s.description.as_str()));
            text.extend(t.scopes.iter().map(|s| s.scope.as_str()));
            toolbar::matches(&words, &text)
        })
        .collect();
    let toolbar = ToolbarView::new(&list)
        .search("A character, or a scope")
        .filter(&list, "Status", "status", STATUSES);
    let code = error.as_ref().map_or(StatusCode::OK, AppError::status);
    Ok(render(
        code,
        &TokensPage {
            shell: loaded.shell,
            toolbar,
            total,
            rows,
            owners: super::plugin_access::own_sources(state, session.account).await?,
            notice,
            error: error.map(|e| e.message().to_owned()),
        },
    ))
}

/// The page again after an action, saying `message` (a toast with htmx).
async fn done(
    state: &AppState,
    session: &CurrentSession,
    headers: &HeaderMap,
    message: &str,
) -> Result<Response, PageError> {
    let (inline, toast) = notice(headers, message);
    let params = current_query(state.site.origin(), headers, PATH);
    let page = tokens_page(state, session, inline, None, &params).await?;
    Ok(match toast {
        Some(toast) => with_toast(page, toast),
        None => page,
    })
}

/// The page again after an action that failed: the reason on the page,
/// and in a toast with htmx.
async fn failed(
    state: &AppState,
    session: &CurrentSession,
    headers: &HeaderMap,
    err: AppError,
) -> Result<Response, PageError> {
    let toast = Toast::problem(err.message().to_owned());
    let params = current_query(state.site.origin(), headers, PATH);
    Ok(with_toast(
        tokens_page(state, session, None, Some(err), &params).await?,
        toast,
    ))
}

/// `GET /tokens`
pub async fn index(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    Query(params): Query<ListParams>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    tokens_page(&state, &session, None, None, &params).await
}

/// `POST /tokens/{character_id}/refresh`
pub async fn refresh(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
    Path(character): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    match tokens::refresh(&state.db, &state.vault, &state.limits, session.account, character).await {
        Ok(Refreshed::Valid) => done(&state, &session, &headers, "Refreshed: the token works.").await,
        Ok(Refreshed::Revoked) => {
            done(
                &state,
                &session,
                &headers,
                "EVE says that token no longer works. Log in with the character again through Add Character.",
            )
            .await
        }
        // The page may now be Guest's (if it was the main), so it's
        // loaded fresh.
        Ok(Refreshed::Sold) => {
            done(
                &state,
                &session,
                &headers,
                "That character now belongs to another EVE account, so it has left yours.",
            )
            .await
        }
        Err(err) => failed(&state, &session, &headers, err).await,
    }
}

/// `POST /tokens/{character_id}/delete`
pub async fn delete(
    State(state): State<AppState>,
    session: Option<CurrentSession>,
    headers: HeaderMap,
    Path(character): Path<i64>,
) -> Result<Response, PageError> {
    let session = session.ok_or_else(AppError::unauthorized)?;
    match tokens::delete(&state.db, &state.vault, session.account, character).await {
        Ok(()) => {
            done(
                &state,
                &session,
                &headers,
                "Token deleted. The character leaves your account in a day unless you log in with it again.",
            )
            .await
        }
        Err(err) => failed(&state, &session, &headers, err).await,
    }
}

#[cfg(test)]
mod tests {
    use super::summary;

    #[test]
    fn scopes_read_in_one_line() {
        assert_eq!(summary(29, 12, 0), "29 scopes · all required granted");
        assert_eq!(summary(29, 12, 3), "29 scopes · 3 required missing");
        assert_eq!(summary(1, 0, 0), "1 scope");
    }
}
