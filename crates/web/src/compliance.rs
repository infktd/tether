//! Scope compliance (F11, F16, N8), Alliance Auth style: each state other
//! than Guest requires scopes on every character of the account. Accounts
//! that fall short keep their state but are flagged: their owners get a
//! checklist, officers a list, and they're out of the Compliant group. Also
//! Corp Stats, which reads covered corporations' member lists with their
//! registered members' tokens and lists members who never registered.

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
use tether_esi::vault::TokenVault;
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

use crate::AppState;
use crate::error::AppError;

/// Job kind: fetch the member list of every covered corporation, with a
/// registered Member character in it.
pub const CORP_STATS_JOB: &str = "compliance.corp_stats";

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(
        CORP_STATS_JOB,
        CORP_STATS_JOB,
        Duration::from_secs(24 * 60 * 60),
    )]
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

/// Whether `character` is registered as a Member's: the account is Member
/// and the character's token carries every scope Member requires. Such a
/// character is one apps' user-scope calls may read (F16).
pub async fn registered_member(
    db: &PgPool,
    account: AccountId,
    character: i64,
) -> Result<bool, sqlx::Error> {
    let current = registration(db, account).await?;
    let member = current
        .target
        .as_ref()
        .is_some_and(|s| s.builtin == Some(Builtin::Member));
    Ok(member
        && current
            .characters
            .iter()
            .any(|c| c.id == character && c.problem.is_none()))
}

/// After a login stored `character`'s token: if that made it a registered
/// Member character (`was_registered` says whether it was one before),
/// every running app with user scopes runs its schedules now, so the
/// pilot doesn't wait for their next tick to see the character in them.
/// Audited as the system's `schedule.run_now`. Not a schedule queued in
/// the last [`crate::plugin_jobs::TRIGGERED_GAP`]: registering alts one
/// after another doesn't sync an app every minute. Its corporation's
/// member list is read now too if there's none yet (Corp Stats). In the
/// background, so the login doesn't wait for it; best effort, a failure is
/// only logged.
pub fn sync_if_newly_registered(
    state: &AppState,
    account: AccountId,
    character: i64,
    was_registered: bool,
) {
    if was_registered {
        return;
    }
    let (db, plugins) = (state.db.clone(), state.plugins.clone());
    tokio::spawn(async move {
        match registered_member(&db, account, character).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                tracing::warn!(character, error = %err, "checking a new registration");
                return;
            }
        }
        if let Err(err) = read_first_member_list(&db, character).await {
            tracing::warn!(character, error = %err, "queueing a first member list");
        }
        let why = json!({ "reason": "character_registered", "character_id": character });
        for running in plugins.all_running() {
            if allowed_plugin_scopes(&running.manifest.capabilities.esi.user).is_empty() {
                continue;
            }
            crate::plugin_jobs::run_app_schedules(
                &db,
                &running.manifest,
                Actor::System,
                &why,
                crate::plugin_jobs::TRIGGERED_GAP,
            )
            .await;
        }
    });
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

// ---- jobs ------------------------------------------------------------------

/// Fetches every covered corporation's member list (or just `only`'s, for
/// Update Now), and names members who never registered. As Alliance
/// Auth's Corporation Stats: any registered Member character in the
/// corporation reads it (Member requires the scope), the one that worked
/// last first; one whose token or access fails is skipped for the next, up
/// to [`MAX_READERS_TRIED`]. An SSO or ESI outage stops the run (retried
/// later) rather than trying every member's token in turn.
pub async fn corp_stats(db: &PgPool, esi: &Esi, vault: &TokenVault) -> Result<usize, JobError> {
    corp_stats_for(db, esi, vault, None).await
}

/// Characters tried per corporation and run: enough to get past a revoked
/// token or two, without every member's token failing the same way
/// spending ESI's shared error budget.
pub const MAX_READERS_TRIED: usize = 5;

pub async fn corp_stats_for(
    db: &PgPool,
    esi: &Esi,
    vault: &TokenVault,
    only: Option<i64>,
) -> Result<usize, JobError> {
    use tether_esi::EsiError;
    use tether_esi::vault::VaultError;
    let corporations = db::covered_corporations(db)
        .await
        .map_err(JobError::retry)?;
    let mut fetched = 0;
    for corporation in corporations
        .into_iter()
        .filter(|c| only.is_none_or(|o| o == *c))
    {
        let readers = db::member_list_readers(db, corporation, scopes::CORP_MEMBERSHIP)
            .await
            .map_err(JobError::retry)?;
        for character in readers.into_iter().take(MAX_READERS_TRIED) {
            let token = match vault
                .access_token(character, &[scopes::CORP_MEMBERSHIP])
                .await
            {
                Ok(token) => token,
                // This character's token: the next one may work.
                Err(
                    err @ (VaultError::NoToken
                    | VaultError::MissingScopes(_)
                    | VaultError::Revoked
                    | VaultError::OwnerChanged),
                ) => {
                    tracing::warn!(corporation, character, error = %err, "Corp Stats token");
                    continue;
                }
                // SSO (or the vault) is down for everyone: try again later.
                Err(err) => {
                    tracing::warn!(corporation, character, error = %err, "Corp Stats stopped: token");
                    return Err(JobError::retry(err));
                }
            };
            let members = match esi.corporation_members(&token, corporation).await {
                Ok(members) => members,
                // Refused for this character (it left, or lost the scope).
                Err(err @ EsiError::Status(401 | 403)) => {
                    tracing::warn!(corporation, character, error = %err, "Corp Stats member list");
                    continue;
                }
                Err(err) => {
                    tracing::warn!(corporation, character, error = %err, "Corp Stats stopped: member list");
                    return Err(JobError::retry(err));
                }
            };
            let mut tx = db.begin().await.map_err(JobError::retry)?;
            db::store_members(&mut tx, corporation, &members, character)
                .await
                .map_err(JobError::retry)?;
            tx.commit().await.map_err(JobError::retry)?;
            // Names for the page, from the cache or ESI (public).
            let mut ids: Vec<i64> = db::unregistered(db, corporation)
                .await
                .map_err(JobError::retry)?
                .into_iter()
                .filter(|(_, name)| name.is_none())
                .map(|(id, _)| id)
                .collect();
            ids.push(corporation);
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

/// A character just registered: if its corporation has no member list
/// yet, reads it now rather than at the next daily run (the job checks
/// the corporation is covered).
async fn read_first_member_list(db: &PgPool, character: i64) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    if let Some(corporation) = db::corporation_without_list(&mut *tx, character).await? {
        tether_db::corpstats::queue_update(&mut tx, corporation).await?;
    }
    tx.commit().await
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    esi: Esi,
    vault: std::sync::Arc<TokenVault>,
) {
    registry.register(CORP_STATS_JOB, move |job| {
        let (db, esi, vault) = (db.clone(), esi.clone(), vault.clone());
        async move {
            let only = job.payload["corporation_id"].as_i64();
            corp_stats_for(&db, &esi, &vault, only).await?;
            Ok(())
        }
    });
}
