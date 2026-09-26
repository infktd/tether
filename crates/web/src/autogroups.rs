//! Auto Groups (Alliance Auth's): for the states a config covers, a group
//! per main's corporation (and alliance), kept by Tether. Groups are
//! Internal and Hidden; nobody edits their members.
//!
//! Evaluating an account's state reconciles its Auto Groups at once:
//! leaving the ones that no longer apply and joining existing ones. A
//! group that doesn't exist yet needs its corporation's name or ticker
//! from ESI, so the hourly (and on-demand) `autogroups.sync` job creates
//! it, renames groups whose corporation changed name, fills them, and
//! drops the empty ones.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::json;
use tether_core::groups::Flags;
use tether_core::states::{EntityKind, Main, StateId};
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::autogroups::{self as db, Config, Kind, Settings, Source};
use tether_db::groups::{self, GroupId};
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

use crate::error::AppError;

pub const SYNC_JOB: &str = "autogroups.sync";
const EVERY: Duration = Duration::from_secs(60 * 60);

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(SYNC_JOB, SYNC_JOB, EVERY)]
}

/// The Auto Groups an account in `state` with `main` should be in:
/// `(config, kind, corporation or alliance id)`.
pub fn desired(
    configs: &[Config],
    state: StateId,
    main: Option<Main>,
) -> BTreeSet<(i64, Kind, i64)> {
    let mut out = BTreeSet::new();
    let Some(affiliation) = main.and_then(|m| m.affiliation) else {
        return out;
    };
    for config in configs.iter().filter(|c| c.states.contains(&state)) {
        if config.settings.corp_groups {
            out.insert((config.id, Kind::Corporation, affiliation.corporation_id));
        }
        if config.settings.alliance_groups
            && let Some(alliance) = affiliation.alliance_id
        {
            out.insert((config.id, Kind::Alliance, alliance));
        }
    }
    out
}

/// Brings one account's Auto Groups in line, in the caller's transaction.
/// Returns whether some group it should be in doesn't exist yet (the sync
/// job creates it).
pub(crate) async fn reconcile_in(
    tx: &mut sqlx::PgConnection,
    account: AccountId,
    state: StateId,
    main: Option<Main>,
    active: bool,
) -> Result<bool, sqlx::Error> {
    let configs = db::configs(&mut *tx).await?;
    // Guest never gets Auto Groups: anyone who signs in is Guest.
    let guest = tether_db::states::get(&mut *tx, state)
        .await?
        .is_none_or(|s| s.is_guest());
    let want = if active && !guest {
        desired(&configs, state, main)
    } else {
        BTreeSet::new()
    };
    let have = db::memberships(&mut *tx, account).await?;
    for m in &have {
        if !want.contains(&(m.config, m.kind, m.entity)) {
            groups::remove_member(&mut *tx, m.group, account).await?;
            record(tx, m.group, account, false).await?;
        }
    }
    let mut missing = false;
    for (config, kind, entity) in &want {
        match db::group_for(&mut *tx, *config, *kind, *entity).await? {
            Some(group) => {
                if groups::add_member(&mut *tx, group, account).await? {
                    record(tx, group, account, true).await?;
                }
            }
            None => missing = true,
        }
    }
    Ok(missing)
}

async fn record(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    account: AccountId,
    added: bool,
) -> Result<(), sqlx::Error> {
    audit::record(
        &mut *tx,
        Actor::System,
        if added {
            "group.member.add"
        } else {
            "group.member.remove"
        },
        Some(&format!("group:{}", group.0)),
        json!({ "account_id": account.0, "reason": "auto group" }),
    )
    .await
}

/// The group's name: prefix, then the corporation's or alliance's name or
/// ticker, spaces replaced if asked (AA's rules), at most 100 characters.
pub fn group_name(settings: &Settings, kind: Kind, name: &str, ticker: &str) -> String {
    let (prefix, source) = match kind {
        Kind::Corporation => (&settings.corp_prefix, settings.corp_source),
        Kind::Alliance => (&settings.alliance_prefix, settings.alliance_source),
    };
    let mut part = match source {
        Source::Name => name.to_owned(),
        Source::Ticker => ticker.to_owned(),
    };
    if settings.replace_spaces {
        part = part.replace(' ', &settings.replace_with);
    }
    format!("{prefix}{part}")
        .chars()
        .take(100)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Creates and renames every config's groups, fills them, and drops the
/// empty ones.
/// Only one sync runs at a time (a session advisory lock).
const SYNC_LOCK: i64 = 0x7465_7468_6572_4147;

pub async fn sync(db_pool: &PgPool, esi: &Esi) -> Result<usize, JobError> {
    let mut lock = db_pool.acquire().await.map_err(JobError::retry)?;
    let got = sqlx::query_scalar!(r#"SELECT pg_try_advisory_lock($1) AS "got!""#, SYNC_LOCK)
        .fetch_one(&mut *lock)
        .await
        .map_err(JobError::retry)?;
    if !got {
        return Ok(0);
    }
    let result = run_sync(db_pool, esi).await;
    sqlx::query_scalar!(r#"SELECT pg_advisory_unlock($1) AS "done!""#, SYNC_LOCK)
        .fetch_one(&mut *lock)
        .await
        .map_err(JobError::retry)?;
    result
}

async fn run_sync(db_pool: &PgPool, esi: &Esi) -> Result<usize, JobError> {
    let configs = db::configs(db_pool).await.map_err(JobError::retry)?;
    if configs.is_empty() {
        return Ok(0);
    }
    let accounts = tether_db::accounts::all_ids(db_pool)
        .await
        .map_err(JobError::retry)?;
    // Who needs which group, and the names to find.
    let mut wanted: BTreeMap<(i64, Kind, i64), ()> = BTreeMap::new();
    for account in &accounts {
        let mut conn = db_pool.acquire().await.map_err(JobError::retry)?;
        let Some(standing) = tether_db::accounts::standing(&mut *conn, *account)
            .await
            .map_err(JobError::retry)?
        else {
            continue;
        };
        let guest = tether_db::states::get(&mut *conn, standing.state)
            .await
            .map_err(JobError::retry)?
            .is_none_or(|s| s.is_guest());
        if !standing.active || guest {
            continue;
        }
        let main = tether_db::states::main(&mut *conn, *account)
            .await
            .map_err(JobError::retry)?;
        for key in desired(&configs, standing.state, main) {
            wanted.insert(key, ());
        }
    }
    let ids: Vec<i64> = wanted
        .keys()
        .map(|(_, _, id)| *id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let names = tether_esi::names::resolve(db_pool, esi, &ids, Priority::Bulk)
        .await
        .map_err(|e| JobError::retry(e.to_string()))?;
    let mut created = 0;
    for (config, kind, entity) in wanted.keys() {
        let Some(settings) = configs
            .iter()
            .find(|c| c.id == *config)
            .map(|c| &c.settings)
        else {
            continue;
        };
        let source = match kind {
            Kind::Corporation => settings.corp_source,
            Kind::Alliance => settings.alliance_source,
        };
        let name = names
            .get(entity)
            .map_or_else(|| format!("{entity}"), |n| n.name.clone());
        let ticker = if source == Source::Ticker {
            let entity_kind = match kind {
                Kind::Corporation => EntityKind::Corporation,
                Kind::Alliance => EntityKind::Alliance,
            };
            match tether_esi::names::ticker(db_pool, esi, *entity, entity_kind, Priority::Bulk)
                .await
            {
                Ok(ticker) => ticker,
                Err(err) => {
                    tracing::warn!(entity, error = %err, "no ticker for an Auto Group yet");
                    continue;
                }
            }
        } else {
            String::new()
        };
        let group_name = group_name(settings, *kind, &name, &ticker);
        if group_name.is_empty() {
            continue;
        }
        let mut tx = db_pool.begin().await.map_err(JobError::retry)?;
        match db::group_for(&mut *tx, *config, *kind, *entity)
            .await
            .map_err(JobError::retry)?
        {
            Some(group) => {
                let current = groups::get(&mut *tx, group)
                    .await
                    .map_err(JobError::retry)?;
                if let Some(current) = current.filter(|g| g.name != group_name)
                    && !groups::name_taken(&mut *tx, &group_name)
                        .await
                        .map_err(JobError::retry)?
                    && !groups::is_reserved(&mut *tx, &group_name)
                        .await
                        .map_err(JobError::retry)?
                {
                    db::rename_group(&mut *tx, group, &group_name)
                        .await
                        .map_err(JobError::retry)?;
                    audit::record(
                        &mut *tx,
                        Actor::System,
                        "group.rename",
                        Some(&format!("group:{}", group.0)),
                        json!({ "from": current.name, "to": group_name, "auto_group": config }),
                    )
                    .await
                    .map_err(JobError::retry)?;
                }
            }
            None => {
                if groups::name_taken(&mut *tx, &group_name)
                    .await
                    .map_err(JobError::retry)?
                    || groups::is_reserved(&mut *tx, &group_name)
                        .await
                        .map_err(JobError::retry)?
                {
                    // Never take over someone's group: its grants would
                    // go to everyone in the corporation.
                    tracing::warn!(name = group_name, "an Auto Group's name is taken; skipped");
                    continue;
                }
                let flags = Flags {
                    internal: true,
                    hidden: true,
                    ..Flags::default()
                };
                let group = match groups::create(
                    &mut *tx,
                    &group_name,
                    &format!("Auto Group: every main in {name}"),
                    flags,
                )
                .await
                {
                    Ok(group) => group,
                    // Taken a moment ago (an admin, say): skipped, like above.
                    Err(err) if crate::error::is_unique_violation(&err) => {
                        tracing::warn!(
                            name = group_name,
                            "an Auto Group's name was taken; skipped"
                        );
                        continue;
                    }
                    Err(err) => return Err(JobError::retry(err)),
                };
                db::insert_group(&mut *tx, group, *config, *kind, *entity)
                    .await
                    .map_err(JobError::retry)?;
                audit::record(
                    &mut *tx,
                    Actor::System,
                    "group.create",
                    Some(&format!("group:{}", group.0)),
                    json!({ "name": group_name, "auto_group": config }),
                )
                .await
                .map_err(JobError::retry)?;
                created += 1;
            }
        }
        tx.commit().await.map_err(JobError::retry)?;
    }
    // Fill them: every account reconciled (the evaluation does it).
    for account in &accounts {
        crate::states::evaluate_account(db_pool, *account)
            .await
            .map_err(JobError::retry)?;
    }
    let mut tx = db_pool.begin().await.map_err(JobError::retry)?;
    for (group, name) in db::remove_empty(&mut *tx).await.map_err(JobError::retry)? {
        audit::record(
            &mut *tx,
            Actor::System,
            "group.delete",
            Some(&format!("group:{}", group.0)),
            json!({ "name": name, "reason": "empty Auto Group" }),
        )
        .await
        .map_err(JobError::retry)?;
    }
    tx.commit().await.map_err(JobError::retry)?;
    Ok(created)
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, esi: Esi) {
    registry.register(SYNC_JOB, move |_job| {
        let (db, esi) = (db.clone(), esi.clone());
        async move {
            let created = sync(&db, &esi).await?;
            tracing::info!(created, "Auto Groups synced");
            Ok(())
        }
    });
}

// ---- admin (callers check admin.groups) -----------------------------------

fn settings_json(settings: &Settings, states: &[StateId]) -> serde_json::Value {
    json!({
        "states": states.iter().map(|s| s.0).collect::<Vec<_>>(),
        "corp_groups": settings.corp_groups,
        "corp_prefix": settings.corp_prefix,
        "corp_source": settings.corp_source.as_str(),
        "alliance_groups": settings.alliance_groups,
        "alliance_prefix": settings.alliance_prefix,
        "alliance_source": settings.alliance_source.as_str(),
        "replace_spaces": settings.replace_spaces,
        "replace_with": settings.replace_with,
    })
}

/// States must exist, and never Guest (anyone who signs in is Guest).
async fn check_states(tx: &mut sqlx::PgConnection, states: &[StateId]) -> Result<(), AppError> {
    let known = tether_db::states::list(&mut *tx).await?;
    for state in states {
        match known.iter().find(|k| k.id == *state) {
            None => return Err(AppError::bad_request("Choose states from the list.")),
            Some(k) if k.is_guest() => {
                return Err(AppError::bad_request(
                    "Auto Groups can't cover Guest: anyone who signs in is Guest.",
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn check(settings: &Settings, states: &[StateId]) -> Result<(), AppError> {
    if !settings.corp_groups && !settings.alliance_groups {
        return Err(AppError::bad_request(
            "Turn on corporation groups, alliance groups or both.",
        ));
    }
    if states.is_empty() {
        return Err(AppError::bad_request("Choose at least one state."));
    }
    for prefix in [&settings.corp_prefix, &settings.alliance_prefix] {
        if prefix.chars().count() > 30 {
            return Err(AppError::bad_request("Prefixes are at most 30 characters."));
        }
    }
    if settings.replace_with.chars().count() > 5 {
        return Err(AppError::bad_request(
            "Replace spaces with at most 5 characters.",
        ));
    }
    Ok(())
}

/// A config's groups hand their permissions to everyone it covers, so
/// changing who it covers needs them all (as for compliance groups).
async fn require_grants(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    config: i64,
) -> Result<(), AppError> {
    // Each group's grants and those of the groups it leads, as for any
    // group whose members change.
    for (group, ..) in db::groups_of(&mut *tx, config).await? {
        crate::groups::require_grants(&mut *tx, actor, group, "change this config").await?;
    }
    Ok(())
}

pub async fn create(
    db_pool: &PgPool,
    actor: AccountId,
    settings: &Settings,
    states: &[StateId],
) -> Result<i64, AppError> {
    check(settings, states)?;
    let mut tx = db_pool.begin().await?;
    check_states(&mut tx, states).await?;
    let id = db::create(&mut tx, settings, states).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "autogroups.create",
        Some(&format!("autogroups:{id}")),
        settings_json(settings, states),
    )
    .await?;
    db::queue_sync(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn update(
    db_pool: &PgPool,
    actor: AccountId,
    id: i64,
    settings: &Settings,
    states: &[StateId],
) -> Result<(), AppError> {
    check(settings, states)?;
    let mut tx = db_pool.begin().await?;
    check_states(&mut tx, states).await?;
    require_grants(&mut tx, actor, id).await?;
    if !db::update(&mut tx, id, settings, states).await? {
        return Err(AppError::not_found("No such Auto Groups config."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "autogroups.update",
        Some(&format!("autogroups:{id}")),
        settings_json(settings, states),
    )
    .await?;
    db::queue_sync(&mut *tx).await?;
    // Narrowing takes members out now, not at the next sync.
    crate::states::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete(db_pool: &PgPool, actor: AccountId, id: i64) -> Result<(), AppError> {
    let mut tx = db_pool.begin().await?;
    let groups: Vec<String> = db::groups_of(&mut *tx, id)
        .await?
        .into_iter()
        .map(|(_, _, _, name)| name)
        .collect();
    if !db::delete(&mut tx, id).await? {
        return Err(AppError::not_found("No such Auto Groups config."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "autogroups.delete",
        Some(&format!("autogroups:{id}")),
        json!({ "groups": groups }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_core::states::Affiliation;

    fn settings() -> Settings {
        Settings {
            corp_groups: true,
            corp_prefix: "Corp ".into(),
            corp_source: Source::Name,
            alliance_groups: true,
            alliance_prefix: "Alliance ".into(),
            alliance_source: Source::Ticker,
            replace_spaces: false,
            replace_with: String::new(),
        }
    }

    #[test]
    fn names_follow_aas_settings() {
        let mut s = settings();
        assert_eq!(
            group_name(&s, Kind::Corporation, "New Miners Union", "NMU"),
            "Corp New Miners Union"
        );
        assert_eq!(
            group_name(&s, Kind::Alliance, "Goonswarm", "CONDI"),
            "Alliance CONDI"
        );
        s.replace_spaces = true;
        s.replace_with = "_".into();
        assert_eq!(
            group_name(&s, Kind::Corporation, "New Miners Union", "NMU"),
            "Corp New_Miners_Union"
        );
    }

    #[test]
    fn only_covered_states_get_groups() {
        let config = Config {
            id: 1,
            settings: settings(),
            states: vec![StateId(1)],
        };
        let main = Some(Main {
            character_id: 7,
            affiliation: Some(Affiliation {
                corporation_id: 100,
                alliance_id: Some(200),
                faction_id: None,
            }),
        });
        let want = desired(std::slice::from_ref(&config), StateId(1), main);
        assert_eq!(
            want.into_iter().collect::<Vec<_>>(),
            [(1, Kind::Corporation, 100), (1, Kind::Alliance, 200)]
        );
        assert!(desired(&[config], StateId(2), main).is_empty());
    }
}
