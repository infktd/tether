//! Scope compliance (F11, F16, N8), Alliance Auth style: each state other
//! than Guest requires scopes on every character of the account. Accounts
//! that fall short keep their state but are flagged: their owners get a
//! checklist, officers a list, and they're out of the Compliant group.
//!
//! Registering for apps, as AA's Member Audit: whoever holds one of an
//! app's permissions registers characters for it, granting its user
//! scopes in one EVE login, and the app reads those characters (and no
//! others). Admins may also require an app's scopes of a state.
//!
//! Also Corp Stats, which reads covered corporations' member lists with
//! their registered members' tokens and lists members who never
//! registered.

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

/// The scopes `state` requires, read in the caller's transaction: its
/// own, and those of the apps it requires (see [`required_apps_in`]).
pub async fn required_in(
    conn: &mut sqlx::PgConnection,
    state: &State,
) -> Result<BTreeSet<String>, sqlx::Error> {
    if state.is_guest() {
        return Ok(BTreeSet::new());
    }
    let admin = db::admin_scopes(&mut *conn, state.id).await?;
    let member = state.builtin == Some(Builtin::Member);
    let mut required = scopes::required(member, &admin);
    for app in db::state_apps(&mut *conn, state.id).await? {
        required.extend(app.scopes);
    }
    Ok(required)
}

/// The apps `state` requires every character of `account` to be
/// registered for (AA's Member Audit compliance), with which of them are.
pub async fn required_apps_in(
    conn: &mut sqlx::PgConnection,
    state: &State,
    account: AccountId,
) -> Result<Vec<scopes::RequiredApp>, sqlx::Error> {
    if state.is_guest() {
        return Ok(Vec::new());
    }
    let apps = db::state_apps(&mut *conn, state.id).await?;
    if apps.is_empty() {
        return Ok(Vec::new());
    }
    let registrations = db::account_app_registrations(&mut *conn, account).await?;
    Ok(apps
        .into_iter()
        .filter(|app| !app.scopes.is_empty())
        .map(|app| scopes::RequiredApp {
            registered: registrations
                .iter()
                .filter(|(plugin, _)| *plugin == app.id)
                .map(|(_, character)| *character)
                .collect(),
            name: app.name,
            scopes: app.scopes.into_iter().collect(),
        })
        .collect())
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
    let apps = required_apps_in(&mut *conn, &state, account).await?;
    let characters: Vec<(i64, scopes::Token)> = db::account_tokens(&mut *conn, account)
        .await?
        .into_iter()
        .map(|c| (c.id, c.token))
        .collect();
    Ok(scopes::check_with_apps(&required, &apps, &characters))
}

/// The user scopes registering for a plugin grants: only those a
/// character endpoint in the plugin ESI catalogue uses. Anything else
/// couldn't be used anyway, and one EVE refuses would stop anyone
/// registering for it.
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

/// Records a plugin's user scopes; re-evaluates everyone if they changed
/// (a state requiring the app requires its scopes).
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
    /// The apps it requires every character to be registered for, by name
    /// (registering for the state registers for them).
    pub apps: Vec<String>,
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
    let (required, apps) = match &target {
        Some(state) => (
            required_in(&mut conn, state).await?,
            required_apps_in(&mut conn, state, account).await?,
        ),
        None => (BTreeSet::new(), Vec::new()),
    };
    let tokens = db::account_tokens(&mut *conn, account).await?;
    let pairs: Vec<(i64, scopes::Token)> = tokens.iter().map(|c| (c.id, c.token.clone())).collect();
    let problems: BTreeMap<i64, Problem> = scopes::check_with_apps(&required, &apps, &pairs)
        .into_iter()
        .collect();
    let characters = tokens
        .into_iter()
        .map(|c| CharacterStatus {
            problem: problems.get(&c.id).cloned(),
            scopes: match c.token {
                scopes::Token::Valid(scopes) | scopes::Token::Revoked(scopes) => scopes,
                scopes::Token::None => Vec::new(),
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
        apps: apps.into_iter().map(|a| a.name).collect(),
        characters,
    })
}

/// Whether `character` is registered as a Member's: the account is Member
/// and the character's token carries every scope Member requires (the
/// corporation member list among them, for Corp Stats).
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

/// The user scopes of a running app, as registering for it grants them;
/// empty for an app that reads no pilot's characters.
pub fn app_scopes(running: &crate::plugins::Running) -> Vec<String> {
    let mut scopes = allowed_plugin_scopes(&running.manifest.capabilities.esi.user);
    scopes.sort();
    scopes.dedup();
    scopes
}

/// The running apps whose characters `character` is among (F16): it is
/// registered for the app, its account holds one of the app's
/// permissions, and its token carries all of the app's user scopes.
pub async fn apps_serving(
    state: &AppState,
    character: i64,
) -> Result<BTreeSet<String>, sqlx::Error> {
    let mut apps = BTreeSet::new();
    for running in state.plugins.all_running() {
        let scopes = app_scopes(&running);
        if scopes.is_empty() {
            continue;
        }
        let id = &running.manifest.plugin.id;
        if db::character_may_serve(&state.db, id, character, &scopes).await? {
            apps.insert(id.clone());
        }
    }
    Ok(apps)
}

/// What a character was before a login stored its token, to tell what the
/// login changed.
#[derive(Debug, Clone, Default)]
pub struct Before {
    /// Registered as a Member's (see [`registered_member`]).
    pub member: bool,
    /// The apps it was one of the characters of (see [`apps_serving`]);
    /// `None` when unknown, which counts as every app.
    pub apps: Option<BTreeSet<String>>,
}

impl Before {
    /// Nothing is new after this login: a re-authentication, or the check
    /// failed (no sync, and nothing fails).
    pub fn everything() -> Self {
        Self {
            member: true,
            apps: None,
        }
    }

    fn had(&self, app: &str) -> bool {
        self.apps.as_ref().is_none_or(|apps| apps.contains(app))
    }
}

/// Where `character` stands, before a login stores its token.
pub async fn before_login(state: &AppState, account: AccountId, character: i64) -> Before {
    let checked = async {
        Ok::<_, sqlx::Error>(Before {
            member: registered_member(&state.db, account, character).await?,
            apps: Some(apps_serving(state, character).await?),
        })
    };
    checked.await.unwrap_or_else(|err| {
        tracing::warn!(character_id = character, error = %err, "checking registration before login");
        Before::everything()
    })
}

/// After a login stored `character`'s token: every running app the login
/// made it one of the characters of runs its schedules now, so the pilot
/// doesn't wait for their next tick to see the character in them.
/// Audited as the system's `schedule.run_now`. Not a schedule queued in
/// the last [`crate::plugin_jobs::TRIGGERED_GAP`]: registering alts one
/// after another doesn't sync an app every minute. If the login made it a
/// registered Member character, its corporation's member list is read now
/// too if there's none yet (Corp Stats). In the background, so the login
/// doesn't wait for it; best effort, a failure is only logged.
pub fn sync_if_newly_registered(
    state: &AppState,
    account: AccountId,
    character: i64,
    before: Before,
) {
    let state = state.clone();
    tokio::spawn(async move {
        if !before.member {
            match registered_member(&state.db, account, character).await {
                Ok(true) => {
                    if let Err(err) = read_first_member_list(&state.db, character).await {
                        tracing::warn!(character, error = %err, "queueing a first member list");
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(character, error = %err, "checking a new registration");
                }
            }
        }
        let now = match apps_serving(&state, character).await {
            Ok(now) => now,
            Err(err) => {
                tracing::warn!(character, error = %err, "checking a new app registration");
                return;
            }
        };
        let why = json!({ "reason": "character_registered", "character_id": character });
        for running in state.plugins.all_running() {
            let id = &running.manifest.plugin.id;
            if !now.contains(id) || before.had(id) {
                continue;
            }
            crate::plugin_jobs::run_app_schedules(
                &state.db,
                &running.manifest,
                Actor::System,
                &why,
                crate::plugin_jobs::TRIGGERED_GAP,
            )
            .await;
        }
    });
}

// ---- registering for an app --------------------------------------------------

/// Where an account stands with one app: which of its characters the app
/// reads (registered for it), and what registering grants.
#[derive(Debug, Clone)]
pub struct AppRegistration {
    pub id: String,
    pub name: String,
    /// The app's user scopes: every one is needed.
    pub scopes: BTreeSet<String>,
    /// Each character, with what it still needs for the app.
    pub characters: Vec<CharacterStatus>,
    /// The account's characters registered for it (read while their token
    /// carries the app's scopes).
    pub registered: BTreeSet<i64>,
}

impl AppRegistration {
    pub fn ready(&self) -> usize {
        self.characters
            .iter()
            .filter(|c| c.problem.is_none())
            .count()
    }
}

/// The account's standing with the app `id`: `None` unless it's running,
/// reads pilots' characters (user scopes), and the account holds one of
/// its permissions (as AA gates apps, whatever the state).
pub async fn app_registration(
    state: &AppState,
    account: AccountId,
    id: &str,
) -> Result<Option<AppRegistration>, sqlx::Error> {
    let Some(running) = state.plugins.running(id) else {
        return Ok(None);
    };
    app_registration_of(state, account, &running).await
}

async fn app_registration_of(
    state: &AppState,
    account: AccountId,
    running: &crate::plugins::Running,
) -> Result<Option<AppRegistration>, sqlx::Error> {
    let scopes: BTreeSet<String> = app_scopes(running).into_iter().collect();
    let id = &running.manifest.plugin.id;
    if scopes.is_empty() || !db::holds_app_permission(&state.db, account, id).await? {
        return Ok(None);
    }
    let tokens = db::account_tokens(&state.db, account).await?;
    let registered: BTreeSet<i64> = db::registered_for_app(&state.db, account, id)
        .await?
        .into_iter()
        .collect();
    let pairs: Vec<(i64, scopes::Token)> = tokens.iter().map(|c| (c.id, c.token.clone())).collect();
    let problems: BTreeMap<i64, Problem> = scopes::check(&scopes, &pairs).into_iter().collect();
    Ok(Some(AppRegistration {
        id: id.clone(),
        name: running.manifest.plugin.name.clone(),
        characters: tokens
            .into_iter()
            .map(|c| CharacterStatus {
                // Not registered for the app, whatever its token carries
                // (aa-memberaudit reads only characters added to it).
                problem: if registered.contains(&c.id) {
                    problems.get(&c.id).cloned()
                } else {
                    Some(Problem::NotRegistered)
                },
                scopes: match c.token {
                    scopes::Token::Valid(scopes) => scopes,
                    _ => Vec::new(),
                },
                id: c.id,
                name: c.name,
                is_main: c.is_main,
            })
            .collect(),
        scopes,
        registered,
    }))
}

/// Every app the account may register characters for, by name.
pub async fn app_registrations(
    state: &AppState,
    account: AccountId,
) -> Result<Vec<AppRegistration>, sqlx::Error> {
    let mut apps = Vec::new();
    for running in state.plugins.all_running() {
        if let Some(app) = app_registration_of(state, account, &running).await? {
            apps.push(app);
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
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
/// account's state requires, and with `app`'s user scopes when registering
/// for an app (one login does both). Back to the checklist afterwards (the
/// app's, for an app).
pub async fn start_register(
    state: &AppState,
    jar: CookieJar,
    account: AccountId,
    app: Option<&str>,
    checklist: bool,
) -> Result<Response, AppError> {
    let current = registration(&state.db, account).await?;
    let mut wanted = current.required;
    let (back, purpose) = match app {
        Some(id) => {
            let app = app_registration(state, account, id)
                .await?
                .ok_or_else(|| AppError::not_found("No app you may register characters for."))?;
            wanted.extend(app.scopes);
            (
                format!("/register?app={}", app.id),
                Purpose::RegisterApp(app.id),
            )
        }
        // From the checklist, which says it registers for the apps the
        // state requires; a plain Add Character registers for none.
        None if checklist => ("/register".to_owned(), Purpose::RegisterForState),
        None => ("/register".to_owned(), Purpose::Register),
    };
    let scopes = ask_scopes(&state.db, account, wanted).await?;
    crate::auth::start_login(state, jar, &back, purpose, &scopes, Some(account)).await
}

/// After a login started from an app's Register Character stored the
/// character's token: registers the character for the app, if the account
/// still holds one of its permissions and the token carries all its scopes
/// (otherwise the app's page says what's missing). Audited.
pub async fn finish_app_registration(
    state: &AppState,
    account: AccountId,
    character: i64,
    plugin: &str,
) -> Result<(), sqlx::Error> {
    let Some(app) = app_registration(state, account, plugin).await? else {
        return Ok(());
    };
    let carries = tether_db::compliance::account_tokens(&state.db, account)
        .await?
        .into_iter()
        .find(|c| c.id == character)
        .is_some_and(|c| scopes::check(&app.scopes, &[(c.id, c.token)]).is_empty());
    if !carries {
        return Ok(());
    }
    let mut tx = state.db.begin().await?;
    if db::register_app_character(&mut *tx, &app.id, character, account).await? {
        audit::record(
            &mut *tx,
            Actor::Account(account),
            "plugin.character_registered",
            Some(&format!("plugin:{}", app.id)),
            json!({ "character_id": character }),
        )
        .await?;
    }
    tx.commit().await
}

/// After a login from the state's checklist stored the character's token:
/// registers it for the apps the state requires, whose scopes it now
/// carries, while the account holds one of the app's permissions (as AA's
/// Member Audit registration needs basic_access; the checklist says so).
/// Audited.
pub async fn finish_state_registration(
    db: &PgPool,
    account: AccountId,
    character: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    for plugin in db::register_for_state_apps(&mut *tx, account, character).await? {
        audit::record(
            &mut *tx,
            Actor::Account(account),
            "plugin.character_registered",
            Some(&format!("plugin:{plugin}")),
            json!({ "character_id": character, "for_state": true }),
        )
        .await?;
    }
    tx.commit().await
}

/// Unregisters one of the account's characters from an app (its Register
/// page): the app stops reading it at once. Audited.
pub async fn unregister_app_character(
    db: &PgPool,
    account: AccountId,
    plugin: &str,
    character: i64,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let removed = db::unregister_app_character(&mut *tx, plugin, character, account).await?;
    if removed {
        audit::record(
            &mut *tx,
            Actor::Account(account),
            "plugin.character_unregistered",
            Some(&format!("plugin:{plugin}")),
            json!({ "character_id": character }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(removed)
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
