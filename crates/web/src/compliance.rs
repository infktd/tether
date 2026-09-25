//! Scope compliance (F11, F16, N8), Alliance Auth style: each state other
//! than Guest requires scopes on every character of the account. Accounts
//! that fall short keep their state but are flagged: their owners get a
//! checklist, officers a list, and they're out of the Compliant group. Also the daily token check, which notices revoked tokens, and Corp
//! Stats, which lists corporation members who never registered.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use axum::response::Response;
use axum_extra::extract::CookieJar;
use serde_json::json;
use tether_core::scopes::{self, Problem};
use tether_core::states::{Builtin, State, StateId};
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::auth::Purpose;
use tether_db::compliance as db;
use tether_db::states as state_db;
use tether_esi::sso::SsoIdentity;
use tether_esi::vault::{TokenVault, VaultError};
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, NewJob, Registry};

use crate::AppState;
use crate::error::AppError;

/// Job kind: confirm stored tokens still work, oldest-checked first.
pub const CHECK_TOKENS_JOB: &str = "compliance.check_tokens";
/// Job kind: fetch the member list of every corporation with an approved
/// Corp Stats source.
pub const CORP_STATS_JOB: &str = "compliance.corp_stats";
/// Each token is checked about once a day.
const CHECK_EVERY_HOURS: i32 = 24;
/// Tokens checked per hourly run: 24 runs cover 12,000 characters a day.
const CHECK_BATCH: i64 = 500;

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![
        ScheduleSpec::new(
            CHECK_TOKENS_JOB,
            CHECK_TOKENS_JOB,
            Duration::from_secs(60 * 60),
        ),
        ScheduleSpec::new(
            CORP_STATS_JOB,
            CORP_STATS_JOB,
            Duration::from_secs(24 * 60 * 60),
        ),
    ]
}

// ---- requirements ----------------------------------------------------------

/// What `state` requires, read in the caller's transaction.
pub async fn required_in(
    conn: &mut sqlx::PgConnection,
    state: &State,
) -> Result<BTreeSet<String>, sqlx::Error> {
    if state.is_guest() {
        return Ok(BTreeSet::new());
    }
    let admin = db::admin_scopes(&mut *conn, state.id).await?;
    let member = state.builtin == Some(Builtin::Member);
    let plugins: Vec<String> = if member {
        db::plugin_scopes(&mut *conn)
            .await?
            .into_iter()
            .flat_map(|p| p.scopes)
            .collect()
    } else {
        Vec::new()
    };
    Ok(scopes::required(member, &plugins, &admin))
}

/// Which of the account's characters fall short of `candidate`'s
/// requirements.
pub async fn problems(
    conn: &mut sqlx::PgConnection,
    account: AccountId,
    candidate: StateId,
) -> Result<Vec<(i64, Problem)>, sqlx::Error> {
    let Some(state) = state_db::get(&mut *conn, candidate).await? else {
        return Ok(Vec::new());
    };
    let required = required_in(&mut *conn, &state).await?;
    if required.is_empty() {
        return Ok(Vec::new());
    }
    let characters: Vec<(i64, scopes::Token)> = db::account_tokens(&mut *conn, account)
        .await?
        .into_iter()
        .map(|c| (c.id, c.token))
        .collect();
    Ok(scopes::check(&required, &characters))
}

/// The user scopes Member may require for a plugin: only those a character
/// endpoint in the plugin ESI catalogue uses. Anything else couldn't be
/// used anyway, and one EVE refuses would stop every Member registering.
pub fn allowed_plugin_scopes(scopes: &[String]) -> Vec<String> {
    scopes
        .iter()
        .filter(|s| is_catalogue_character_scope(s))
        .cloned()
        .collect()
}

pub fn is_catalogue_character_scope(scope: &str) -> bool {
    tether_esi::plugin::ENDPOINTS
        .iter()
        .any(|e| e.about == tether_esi::plugin::About::Character && e.scope == scope)
}

/// Records a plugin's user scopes; re-evaluates everyone if Member's
/// requirements changed.
pub async fn sync_plugin_scopes(
    db: &PgPool,
    plugin_id: &str,
    scopes: &[String],
) -> Result<(), sqlx::Error> {
    let scopes = allowed_plugin_scopes(scopes);
    let mut tx = db.begin().await?;
    if db::set_plugin_scopes(&mut *tx, plugin_id, &scopes).await? {
        audit::record(
            &mut *tx,
            Actor::System,
            "plugin.user_scopes_changed",
            Some(&format!("plugin:{plugin_id}")),
            json!({ "scopes": scopes }),
        )
        .await?;
        crate::states::enqueue_evaluate_all(&mut *tx).await?;
    }
    tx.commit().await
}

// ---- registering -----------------------------------------------------------

/// A character on the checklist.
#[derive(Debug, Clone)]
pub struct CharacterStatus {
    pub id: i64,
    pub name: String,
    pub is_main: bool,
    /// What it still needs; `None` when it's done.
    pub problem: Option<Problem>,
    /// Scopes its token carries.
    pub scopes: Vec<String>,
}

/// Where an account stands.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The account's state; `None` for Guest.
    pub target: Option<State>,
    /// Not every character is registered with the state's scopes.
    pub flagged: bool,
    pub required: BTreeSet<String>,
    pub characters: Vec<CharacterStatus>,
}

impl Registration {
    pub fn done(&self) -> bool {
        self.characters.iter().all(|c| c.problem.is_none())
    }
}

pub async fn registration(db: &PgPool, account: AccountId) -> Result<Registration, sqlx::Error> {
    let mut conn = db.acquire().await?;
    let flagged = db::not_compliant_state(&mut *conn, account)
        .await?
        .is_some();
    let target = state_db::account_state(&mut *conn, account)
        .await?
        .filter(|s| !s.is_guest());
    let required = match &target {
        Some(state) => required_in(&mut conn, state).await?,
        None => BTreeSet::new(),
    };
    let tokens = db::account_tokens(&mut *conn, account).await?;
    let pairs: Vec<(i64, scopes::Token)> = tokens.iter().map(|c| (c.id, c.token.clone())).collect();
    let problems: BTreeMap<i64, Problem> = scopes::check(&required, &pairs).into_iter().collect();
    let characters = tokens
        .into_iter()
        .map(|c| CharacterStatus {
            problem: problems.get(&c.id).cloned(),
            scopes: match c.token {
                scopes::Token::Valid(scopes) => scopes,
                _ => Vec::new(),
            },
            id: c.id,
            name: c.name,
            is_main: c.is_main,
        })
        .collect();
    Ok(Registration {
        target,
        flagged,
        required,
        characters,
    })
}

/// The scopes to ask SSO for: `wanted`, plus every scope the account's
/// tokens already carry, so a new login never narrows an old grant.
pub async fn ask_scopes(
    db: &PgPool,
    account: AccountId,
    wanted: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, sqlx::Error> {
    let mut all: BTreeSet<String> = tether_db::plugin_esi::account_token_scopes(db, account)
        .await?
        .into_iter()
        .collect();
    all.extend(wanted);
    Ok(all.into_iter().collect())
}

/// Off to EVE SSO to register (or add) a character with the scopes the
/// account's state requires. Back to the checklist afterwards.
pub async fn start_register(
    state: &AppState,
    jar: CookieJar,
    account: AccountId,
) -> Result<Response, AppError> {
    let current = registration(&state.db, account).await?;
    let scopes = ask_scopes(&state.db, account, current.required).await?;
    crate::auth::start_login(
        state,
        jar,
        "/register",
        Purpose::Register,
        &scopes,
        Some(account),
    )
    .await
}

// ---- Corp Stats sources ----------------------------------------------------

/// Off to EVE SSO to offer a character's corporation member list.
pub async fn start_corp_offer(
    state: &AppState,
    jar: CookieJar,
    account: AccountId,
) -> Result<Response, AppError> {
    let scopes = ask_scopes(&state.db, account, [scopes::CORP_MEMBERSHIP.to_owned()]).await?;
    crate::auth::start_login(
        state,
        jar,
        "/profile",
        Purpose::CorpSource,
        &scopes,
        Some(account),
    )
    .await
}

/// After a Corp Stats offer login: records it if the character is the
/// account's and SSO granted the scope.
pub async fn finish_corp_offer(
    state: &AppState,
    account: AccountId,
    identity: &SsoIdentity,
) -> Result<(), AppError> {
    if !identity.scopes.iter().any(|s| s == scopes::CORP_MEMBERSHIP) {
        return Err(AppError::bad_request(format!(
            "EVE didn't grant {}. An admin may need to enable it on Tether's EVE application \
             (developers.eveonline.com).",
            scopes::CORP_MEMBERSHIP
        )));
    }
    if tether_db::plugin_esi::character_account(&state.db, identity.character_id).await?
        != Some(account)
    {
        return Err(AppError::bad_request(
            "That character isn't on your account.",
        ));
    }
    let mut tx = state.db.begin().await?;
    if db::offer_corp_source(&mut *tx, identity.character_id, account).await? {
        audit::record(
            &mut *tx,
            Actor::Account(account),
            "corp_stats.offered",
            Some(&format!("character:{}", identity.character_id)),
            json!({}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The owner withdraws an offer (or an approved source).
pub async fn withdraw_corp_source(
    state: &AppState,
    account: AccountId,
    character: i64,
) -> Result<(), AppError> {
    if tether_db::plugin_esi::character_account(&state.db, character).await? != Some(account) {
        return Err(AppError::not_found("That character isn't on your account."));
    }
    remove(state, account, character, "corp_stats.withdrawn").await
}

pub async fn remove_corp_source(
    state: &AppState,
    admin: AccountId,
    character: i64,
) -> Result<(), AppError> {
    remove(state, admin, character, "corp_stats.removed").await
}

async fn remove(
    state: &AppState,
    actor: AccountId,
    character: i64,
    action: &str,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    if !db::remove_corp_source(&mut *tx, character).await? {
        return Err(AppError::not_found(
            "That character isn't a Corp Stats source.",
        ));
    }
    // Its list goes at once, unless another source keeps it current.
    db::prune_member_lists(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        action,
        Some(&format!("character:{character}")),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An admin approves an offer for the character's current corporation,
/// and the member list is fetched straight away.
pub async fn approve_corp_source(
    state: &AppState,
    admin: AccountId,
    character: i64,
) -> Result<(), AppError> {
    let mut tx = state.db.begin().await?;
    let corporation = db::approve_corp_source(&mut *tx, character, admin)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "That character wasn't offered, or its corporation isn't known yet.",
            )
        })?;
    // Corp Stats is for corporations a state covers (PRD): nobody else's
    // roster is stored here.
    if !db::corporation_covered(&mut *tx, corporation).await? {
        return Err(AppError::bad_request(
            "Nobody in that corporation is in a state other than Guest, so its member list isn't needed.",
        ));
    }
    audit::record(
        &mut *tx,
        Actor::Account(admin),
        "corp_stats.approved",
        Some(&format!("character:{character}")),
        json!({ "corporation_id": corporation }),
    )
    .await?;
    tether_jobs::enqueue(&mut *tx, NewJob::new(CORP_STATS_JOB, json!({}))).await?;
    tx.commit().await?;
    Ok(())
}

// ---- jobs ------------------------------------------------------------------

/// Confirms up to a batch of tokens still refresh. A revoked one is
/// marked by the vault; its account is re-evaluated at once.
pub async fn check_tokens(db: &PgPool, vault: &TokenVault) -> Result<usize, JobError> {
    let due = db::tokens_due(db, CHECK_EVERY_HOURS, CHECK_BATCH)
        .await
        .map_err(JobError::retry)?;
    let mut revoked = 0;
    for character_id in &due {
        match vault.verify(*character_id).await {
            Ok(_) => {}
            Err(VaultError::Revoked) => {
                revoked += 1;
                if let Some(account) = tether_db::plugin_esi::character_account(db, *character_id)
                    .await
                    .map_err(JobError::retry)?
                {
                    crate::states::evaluate_account(db, account)
                        .await
                        .map_err(JobError::retry)?;
                }
            }
            // SSO is down or not set up: try the rest next hour.
            Err(VaultError::Unavailable(_) | VaultError::NotConfigured) => break,
            Err(err) => {
                tracing::warn!(character_id, error = %err, "token check");
            }
        }
        db::mark_checked(db, *character_id)
            .await
            .map_err(JobError::retry)?;
    }
    // Tokens found revoked elsewhere (a plugin call, Corp Stats) since.
    for account in db::compliant_with_revoked(db)
        .await
        .map_err(JobError::retry)?
    {
        crate::states::evaluate_account(db, account)
            .await
            .map_err(JobError::retry)?;
    }
    tracing::info!(checked = due.len(), revoked, "token check done");
    Ok(due.len())
}

/// Fetches every covered corporation's member list with one of its
/// approved sources, and names members who never registered.
pub async fn corp_stats(db: &PgPool, esi: &Esi, vault: &TokenVault) -> Result<usize, JobError> {
    let sources = db::corp_sources(db).await.map_err(JobError::retry)?;
    let mut by_corporation: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for source in sources.iter().filter(|s| s.in_use()) {
        if let Some(corporation) = source.approved_corporation {
            by_corporation
                .entry(corporation)
                .or_default()
                .push(source.character_id);
        }
    }
    let mut fetched = 0;
    for (corporation, characters) in &by_corporation {
        // Only corporations a state covers; others' lists are pruned below.
        if !db::corporation_covered(db, *corporation)
            .await
            .map_err(JobError::retry)?
        {
            continue;
        }
        for character in characters {
            let token = match vault
                .access_token(*character, &[scopes::CORP_MEMBERSHIP])
                .await
            {
                Ok(token) => token,
                Err(err) => {
                    tracing::warn!(character, error = %err, "Corp Stats source token");
                    continue;
                }
            };
            let members = match esi.corporation_members(&token, *corporation).await {
                Ok(members) => members,
                Err(err) => {
                    tracing::warn!(corporation, character, error = %err, "Corp Stats member list");
                    continue;
                }
            };
            let mut tx = db.begin().await.map_err(JobError::retry)?;
            db::store_members(&mut tx, *corporation, &members)
                .await
                .map_err(JobError::retry)?;
            tx.commit().await.map_err(JobError::retry)?;
            // Names for the page, from the cache or ESI (public).
            let mut ids: Vec<i64> = db::unregistered(db, *corporation)
                .await
                .map_err(JobError::retry)?
                .into_iter()
                .filter(|(_, name)| name.is_none())
                .map(|(id, _)| id)
                .collect();
            ids.push(*corporation);
            if let Err(err) = tether_esi::names::resolve(db, esi, &ids, Priority::Bulk).await {
                tracing::warn!(corporation, error = %err, "Corp Stats names");
            }
            fetched += 1;
            break;
        }
    }
    db::prune_member_lists(db).await.map_err(JobError::retry)?;
    tracing::info!(corporations = fetched, "Corp Stats refreshed");
    Ok(fetched)
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    esi: Esi,
    vault: std::sync::Arc<TokenVault>,
) {
    let (check_db, check_vault) = (db.clone(), vault.clone());
    registry.register(CHECK_TOKENS_JOB, move |_job| {
        let (db, vault) = (check_db.clone(), check_vault.clone());
        async move {
            check_tokens(&db, &vault).await?;
            Ok(())
        }
    });
    registry.register(CORP_STATS_JOB, move |_job| {
        let (db, esi, vault) = (db.clone(), esi.clone(), vault.clone());
        async move {
            corp_stats(&db, &esi, &vault).await?;
            Ok(())
        }
    });
}
