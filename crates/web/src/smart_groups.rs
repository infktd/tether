//! Secure Groups (aa-securegroups, AA's "Smart Groups"): groups whose
//! members Tether keeps by filters. An hourly sweep (and one after any
//! change) adds everyone who passes to auto groups, and removes members who
//! stop passing, at once or when their grace period ends. Joining or
//! requesting a smart group needs passing it. Everything is audited and in
//! the group's Audit Log.
//!
//! Changing a smart group is changing who is in it, so it needs what adding
//! members would: the group's grants, and the owner for Restricted groups.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use serde_json::json;
use tether_core::smart::{Facts, Filter, Rule, failing};
use tether_core::states::StateId;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId, RequestType};
use tether_db::smart_groups::{self as db, Settings};
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, Registry};

use crate::error::AppError;

pub const SWEEP_JOB: &str = "smart_groups.sweep";
const EVERY: Duration = Duration::from_secs(60 * 60);
/// Birthdays asked for per sweep, for the character age filter.
const BIRTHDAYS_PER_SWEEP: i64 = 200;
/// Most filters on one group, and ids in one filter.
pub const MAX_FILTERS: usize = 20;
pub const MAX_IDS: usize = 50;

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(SWEEP_JOB, SWEEP_JOB, EVERY)]
}

pub fn register_jobs(registry: &mut Registry, db: PgPool, esi: Esi) {
    registry.register(SWEEP_JOB, move |_job| {
        let (db, esi) = (db.clone(), esi.clone());
        async move {
            let changed = sweep(&db, &esi).await.map_err(JobError::retry)?;
            tracing::info!(changed, "Secure Groups swept");
            Ok(())
        }
    });
}

/// Queues a sweep unless one is waiting.
pub async fn queue_sweep<'e>(executor: impl sqlx::PgExecutor<'e>) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO core.jobs (kind, payload, max_attempts)
        SELECT $1, '{}', 5
        WHERE NOT EXISTS (SELECT 1 FROM core.jobs WHERE kind = $1 AND state = 'queued')
        "#,
        SWEEP_JOB
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Names for filter descriptions: states, groups, corporations and
/// alliances.
#[derive(Debug, Default)]
pub struct Names {
    states: HashMap<i64, String>,
    /// `(name, internal)`.
    groups: HashMap<i64, (String, bool)>,
    entities: HashMap<i64, String>,
    /// For pilots, not admins: Internal groups stay unnamed.
    public: bool,
}

impl Names {
    /// Names for admins (`public` false) or for pilots (Internal groups
    /// shown as "a private group").
    pub async fn load(
        conn: &mut sqlx::PgConnection,
        rules: &[Rule],
        public: bool,
    ) -> Result<Self, sqlx::Error> {
        let mut entities: Vec<i64> = Vec::new();
        for rule in rules {
            if let Filter::MainAffiliation { entities: e }
            | Filter::AnyAffiliation { entities: e } = &rule.filter
            {
                entities.extend(e);
            }
        }
        let names = sqlx::query!(
            "SELECT id, name FROM core.entity_names WHERE id = ANY($1)",
            &entities
        )
        .fetch_all(&mut *conn)
        .await?;
        Ok(Self {
            states: sqlx::query!("SELECT id, name FROM core.states")
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .map(|r| (r.id, r.name))
                .collect(),
            groups: sqlx::query!("SELECT id, name, internal FROM core.groups")
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .map(|r| (r.id, (r.name, r.internal)))
                .collect(),
            entities: names.into_iter().map(|r| (r.id, r.name)).collect(),
            public,
        })
    }

    fn list(map: &HashMap<i64, String>, ids: &[i64]) -> String {
        ids.iter()
            .map(|id| map.get(id).cloned().unwrap_or_else(|| id.to_string()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn group_list(&self, ids: &[i64]) -> String {
        ids.iter()
            .map(|id| match self.groups.get(id) {
                Some((_, true)) if self.public => "a private group".to_owned(),
                Some((name, _)) => name.clone(),
                None => id.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// "main in Pandemic Horde", "not in any of: Spies".
    pub fn describe(&self, rule: &Rule) -> String {
        let text = match &rule.filter {
            Filter::State { states } => format!("state is {}", Self::list(&self.states, states)),
            Filter::MainAffiliation { entities } => {
                format!("main in {}", Self::list(&self.entities, entities))
            }
            Filter::AnyAffiliation { entities } => {
                format!("a character in {}", Self::list(&self.entities, entities))
            }
            Filter::CharacterAge { days } => format!("main at least {days} days old"),
            Filter::Groups { groups, all } => format!(
                "in {} of: {}",
                if *all { "all" } else { "any" },
                self.group_list(groups)
            ),
            Filter::Compliant {} => "compliant".to_owned(),
            Filter::App {
                label,
                sum,
                at_least,
                ..
            } => {
                if *sum {
                    format!("{label}: at least {at_least}")
                } else {
                    label.clone()
                }
            }
        };
        if rule.reversed {
            format!("not {text}")
        } else {
            text
        }
    }
}

/// Adds apps' filter values to the facts; returns which settings have
/// fresh values.
async fn fill_app(
    conn: &mut sqlx::PgConnection,
    facts: &mut HashMap<AccountId, Facts>,
    only: Option<AccountId>,
) -> Result<BTreeSet<String>, sqlx::Error> {
    for (account, key, highest, total, reported) in db::app_values(&mut *conn, only).await? {
        if let Some(f) = facts.get_mut(&account) {
            f.app.insert(key, (highest, total, reported));
        }
    }
    db::app_keys_known(&mut *conn).await
}

fn uses_app(rules: &[Rule]) -> bool {
    rules.iter().any(|r| matches!(r.filter, Filter::App { .. }))
}

/// An app filter with no fresh values (the app is gone, or hasn't reported
/// lately): the group can't be judged, so it fails closed.
fn unknown_app(rules: &[Rule], known: &BTreeSet<String>) -> bool {
    rules.iter().any(|r| match &r.filter {
        Filter::App {
            plugin,
            name,
            config,
            ..
        } => !known.contains(&tether_core::smart::app_key(plugin, name, config)),
        _ => false,
    })
}

/// Refuses an account a smart group it doesn't pass, naming why (Internal
/// groups unnamed). Ordinary groups pass. Runs in the caller's
/// transaction. Callers decide first that the group is one the account
/// may see and join, so this never reveals a group.
pub async fn check(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    account: AccountId,
) -> Result<(), AppError> {
    if db::settings(&mut *tx, group).await?.is_none() {
        return Ok(());
    }
    let (rules, broken) = db::rules(&mut *tx, group).await?;
    if !broken.is_empty() {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "This group's requirements can't be checked right now: an admin needs to look at them.",
        ));
    }
    let mut facts = db::facts(&mut *tx, Some(&[account])).await?;
    let known = if uses_app(&rules) {
        fill_app(&mut *tx, &mut facts, Some(account)).await?
    } else {
        BTreeSet::new()
    };
    if unknown_app(&rules, &known) {
        return Err(AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "This group's requirements can't be checked right now: an app hasn't reported yet.",
        ));
    }
    let Some(facts) = facts.get(&account) else {
        return Err(AppError::forbidden());
    };
    let failed = failing(&rules, facts);
    if failed.is_empty() {
        return Ok(());
    }
    let names = Names::load(tx, &rules, true).await?;
    Err(AppError::new(
        axum::http::StatusCode::FORBIDDEN,
        format!(
            "You don't meet this group's requirements: {}.",
            failed
                .iter()
                .map(|r| names.describe(r))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    ))
}

/// Fills in mains' birthdays for the character age filter, a batch at a
/// time. A character ESI won't answer for goes to the back of the queue;
/// a run of failures means ESI itself is in trouble, and the rest wait.
async fn fill_birthdays(db: &PgPool, esi: &Esi) -> Result<(), sqlx::Error> {
    if !db::uses_age(db).await? {
        return Ok(());
    }
    let mut failures = 0;
    for character in db::mains_without_birthday(db, BIRTHDAYS_PER_SWEEP).await? {
        match esi.character_birthday(character, Priority::Bulk).await {
            Ok(birthday) => db::set_birthday(db, character, birthday).await?,
            Err(err) => {
                tracing::info!(character, error = %err, "birthday not read");
                db::birthday_checked(db, character).await?;
                failures += 1;
                if failures >= 5 {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Brings every smart group in line with its filters. Returns how many
/// memberships changed.
pub async fn sweep(db: &PgPool, esi: &Esi) -> Result<usize, sqlx::Error> {
    let groups = db::all(db).await?;
    if groups.is_empty() {
        return Ok(0);
    }
    fill_birthdays(db, esi).await?;
    // Read once, outside any group's lock; adds are checked again as they
    // happen.
    let mut facts = db::facts(db, None).await?;
    if let Err(err) = db::prune_app_values(db).await {
        tracing::warn!(error = %err, "pruning app filter values failed");
    }
    // An app's values that can't be read leave only its groups alone.
    let known = match fill_app(&mut *db.acquire().await?, &mut facts, None).await {
        Ok(known) => known,
        Err(err) => {
            tracing::warn!(error = %err, "app filter values unreadable; their groups wait");
            BTreeSet::new()
        }
    };
    let guest: i64 = sqlx::query_scalar!(r#"SELECT core.guest_state() AS "g!""#)
        .fetch_one(db)
        .await?;
    let mut changed = 0;
    for (group, _) in groups {
        if let Swept::Changed(n) =
            sweep_one(db, group, &facts, &known, StateId(guest), None).await?
        {
            changed += n;
        }
    }
    Ok(changed)
}

/// What sweeping one group came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Swept {
    /// Judged: this many memberships changed.
    Changed(usize),
    /// Left alone: gone, not smart (any more), kept another way, or with
    /// a filter that can't be judged now.
    Skipped,
}

/// Brings one group in line with its filters, under its lock. With an
/// actor (Check now), the changes and the check itself are audited as
/// theirs, in the same transaction; otherwise as the system's.
async fn sweep_one(
    db: &PgPool,
    group: GroupId,
    facts: &HashMap<AccountId, Facts>,
    known: &BTreeSet<String>,
    guest: StateId,
    actor: Option<AccountId>,
) -> Result<Swept, sqlx::Error> {
    let by = actor.map_or(Actor::System, Actor::Account);
    let mut tx = db.begin().await?;
    // One sweep of a group at a time, and no edits under it.
    if !groups::lock(&mut tx, group, true).await? {
        return Ok(Swept::Skipped);
    }
    let Some(found) = groups::get(&mut *tx, group).await? else {
        return Ok(Swept::Skipped);
    };
    // Tether keeps those groups another way.
    if found.compliance || tether_db::autogroups::is_auto(&mut *tx, group).await? {
        return Ok(Swept::Skipped);
    }
    // Read under the lock: the settings as they are now, not as they were
    // when the sweep (or Check now) started.
    let Some(settings) = db::settings(&mut *tx, group).await? else {
        return Ok(Swept::Skipped);
    };
    let (rules, broken) = db::rules(&mut *tx, group).await?;
    if !broken.is_empty() {
        tracing::warn!(
            group = group.0,
            ?broken,
            "smart group has filters that don't read; left alone"
        );
        return Ok(Swept::Skipped);
    }
    if unknown_app(&rules, known) {
        tracing::warn!(
            group = group.0,
            "an app filter has no fresh values; left alone"
        );
        return Ok(Swept::Skipped);
    }
    let allowed = groups::allowed_states(&mut *tx, group).await?;
    let members: BTreeSet<AccountId> = db::member_ids(&mut *tx, group).await?.into_iter().collect();
    db::clear_stale_grace(&mut *tx, group).await?;
    let grace = db::grace(&mut *tx, group).await?;
    let names = Names::load(&mut tx, &rules, true).await?;
    let now = chrono::Utc::now();
    let mut changed = 0;
    // `None`: can't be in the group at all (no main, deactivated,
    // or a state it doesn't allow); else the filters failed.
    let judge = |account: &AccountId| -> Option<Vec<&Rule>> {
        let f = facts.get(account)?;
        if !tether_core::groups::state_allowed(&allowed, StateId(f.state)) {
            return None;
        }
        Some(failing(&rules, f))
    };
    for account in &members {
        let (why, eligible) = match judge(account) {
            Some(failed) if failed.is_empty() => {
                if db::end_grace(&mut *tx, group, *account).await? {
                    audit::record(
                        &mut *tx,
                        by,
                        "smart_group.grace_end",
                        Some(&format!("group:{}", group.0)),
                        json!({ "account_id": account.0, "reason": "passes again" }),
                    )
                    .await?;
                }
                continue;
            }
            Some(failed) => (
                failed
                    .iter()
                    .map(|r| names.describe(r))
                    .collect::<Vec<_>>()
                    .join("; "),
                true,
            ),
            None => ("not eligible for this group".to_owned(), false),
        };
        // Ineligible accounts (deactivated, without a main, a state the group
        // doesn't allow) leave at once; failing filters gets the grace.
        let remove_now = !eligible
            || settings.grace_days == 0
            || grace.get(account).is_some_and(|since| {
                *since + chrono::Duration::days(i64::from(settings.grace_days)) <= now
            });
        if !remove_now {
            if !grace.contains_key(account) {
                db::start_grace(&mut *tx, group, *account).await?;
                if settings.notify {
                    let date = (now + chrono::Duration::days(i64::from(settings.grace_days)))
                        .format("%Y-%m-%d")
                        .to_string();
                    crate::notifications::smart_failing(
                        &mut tx,
                        *account,
                        &found.name,
                        &why,
                        Some(&date),
                    )
                    .await?;
                }
                audit::record(
                    &mut *tx,
                    by,
                    "smart_group.grace",
                    Some(&format!("group:{}", group.0)),
                    json!({ "account_id": account.0, "failing": why, "days": settings.grace_days }),
                )
                .await?;
            }
            continue;
        }
        groups::remove_member(&mut *tx, group, *account).await?;
        db::end_grace(&mut *tx, group, *account).await?;
        groups::log(&mut *tx, group, RequestType::Removed, true, *account, None).await?;
        audit::record(
            &mut *tx,
            by,
            "group.member.remove",
            Some(&format!("group:{}", group.0)),
            json!({ "account_id": account.0, "reason": "smart group", "failing": why }),
        )
        .await?;
        if settings.notify && eligible {
            crate::notifications::smart_failing(&mut tx, *account, &found.name, &why, None).await?;
        }
        changed += 1;
    }
    // Auto groups take everyone who passes; never on no filters at all
    // (that would be everyone), never Guest unless the group names it, and
    // never a public state (anyone with a main can be in one, and it may
    // have become public after the group named it).
    let public: Vec<StateId> = tether_db::states::list(&mut *tx)
        .await?
        .into_iter()
        .filter(|s| s.public)
        .map(|s| s.id)
        .collect();
    if settings.auto_join && !rules.is_empty() {
        for (account, f) in facts {
            if members.contains(account)
                || (StateId(f.state) == guest && !allowed.contains(&guest))
                || public.contains(&StateId(f.state))
                || !matches!(judge(account), Some(failed) if failed.is_empty())
            {
                continue;
            }
            if !db::add_if_eligible(&mut *tx, group, *account).await? {
                continue;
            }
            groups::log(&mut *tx, group, RequestType::Join, true, *account, None).await?;
            audit::record(
                &mut *tx,
                by,
                "group.member.add",
                Some(&format!("group:{}", group.0)),
                json!({ "account_id": account.0, "reason": "smart group" }),
            )
            .await?;
            if settings.notify {
                crate::notifications::smart_added(&mut tx, *account, &found.name).await?;
            }
            changed += 1;
        }
    }
    sqlx::query!(
        "UPDATE core.smart_groups SET swept_at = now() WHERE group_id = $1",
        group.0
    )
    .execute(&mut *tx)
    .await?;
    if actor.is_some() {
        audit::record(
            &mut *tx,
            by,
            "smart_group.check_now",
            Some(&format!("group:{}", group.0)),
            json!({ "changed": changed }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Swept::Changed(changed))
}

// ---- admin (callers check admin.groups) -----------------------------------

/// Check now waits this long after the group was last judged (by the
/// sweep or a Check now): each reads every account's facts.
const CHECK_GAP: chrono::Duration = chrono::Duration::seconds(15);

/// Check now (aa-securegroups' "Run check"): brings one smart group in
/// line with its filters at once, as the hourly sweep does, audited as the
/// admin's (the check and every change it makes, in one transaction).
/// Returns how many memberships changed. Birthdays ESI hasn't told Tether
/// yet wait for the sweep (an admin's click never waits on a batch of ESI
/// calls). Nothing about the filters changes, so it needs only
/// `admin.groups`, like the sweep it hurries.
pub async fn check_now(db: &PgPool, actor: AccountId, group: GroupId) -> Result<usize, AppError> {
    if db::settings(db, group).await?.is_none() {
        return Err(AppError::bad_request(
            "Only a smart group has filters to check.",
        ));
    }
    if db::swept_at(db, group)
        .await?
        .is_some_and(|at| chrono::Utc::now() - at < CHECK_GAP)
    {
        return Err(AppError::too_many_requests(
            u64::try_from(CHECK_GAP.num_seconds()).unwrap_or(15),
        ));
    }
    let (rules, broken) = db::rules(db, group).await?;
    if !broken.is_empty() {
        return Err(AppError::bad_request(
            "A filter no longer reads, so Tether leaves this group alone: delete it, then check \
             again.",
        ));
    }
    let mut facts = db::facts(db, None).await?;
    let known = fill_app(&mut *db.acquire().await?, &mut facts, None).await?;
    if unknown_app(&rules, &known) {
        return Err(AppError::bad_request(
            "An app filter has no answers from its app in the last two days, so Tether can't \
             judge this group now.",
        ));
    }
    let guest: i64 = sqlx::query_scalar!(r#"SELECT core.guest_state() AS "g!""#)
        .fetch_one(db)
        .await?;
    match sweep_one(db, group, &facts, &known, StateId(guest), Some(actor)).await? {
        Swept::Changed(changed) => Ok(changed),
        Swept::Skipped => Err(AppError::bad_request(
            "Tether keeps this group's members another way, or its filters changed just now: \
             reload the page and check again.",
        )),
    }
}

/// One filter as it judges one account: `None` when it can't say (an app
/// hasn't answered lately, or the filter no longer reads).
pub struct FilterCheck {
    pub text: String,
    pub passes: Option<bool>,
}

/// aa-securegroups' "Check": how a smart group's filters judge one
/// account, for admins.
pub struct Explained {
    pub account: AccountId,
    /// The account's main.
    pub name: String,
    pub member: bool,
    /// In the grace period since then.
    pub grace_since: Option<chrono::DateTime<chrono::Utc>>,
    /// Why the account can't be in the group whatever the filters say.
    pub blocked: Option<String>,
    pub filters: Vec<FilterCheck>,
    /// Blocked by nothing, and every filter passes.
    pub passes: bool,
}

/// How `group`'s filters judge `account` now, for `actor`, audited (it
/// shows an account's main, standing and filter results). Members and
/// those in their grace period are anyone's with `admin.groups` to check;
/// anyone else needs `anyone` (the checker holds `admin.users`, who can
/// look up any account already). Changes nothing else.
pub async fn explain(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
    account: AccountId,
    anyone: bool,
) -> Result<Explained, AppError> {
    if db::settings(db, group).await?.is_none() {
        return Err(AppError::bad_request(
            "Only a smart group has filters to check.",
        ));
    }
    let member = db::member_ids(db, group).await?.contains(&account);
    let grace_since = db::grace(db, group).await?.get(&account).copied();
    if !member && grace_since.is_none() && !anyone {
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "They aren't in this group. Checking anyone else needs the Users permission \
             (admin.users).",
        ));
    }
    audit::record(
        db,
        Actor::Account(actor),
        "smart_group.check",
        Some(&format!("group:{}", group.0)),
        json!({ "account_id": account.0 }),
    )
    .await?;
    let found = tether_db::accounts::get(db, account)
        .await?
        .ok_or_else(|| AppError::not_found("No such account."))?;
    let name = found
        .main
        .as_ref()
        .or(found.characters.first())
        .map(|c| c.name.clone())
        .unwrap_or_else(|| format!("Account {}", account.0));
    let mut conn = db.acquire().await?;
    let (rules, broken) = db::rules(&mut *conn, group).await?;
    let mut facts = db::facts(&mut *conn, Some(&[account])).await?;
    let known = if uses_app(&rules) {
        fill_app(&mut conn, &mut facts, Some(account)).await?
    } else {
        BTreeSet::new()
    };
    let names = Names::load(&mut conn, &rules, false).await?;
    let facts = facts.get(&account);
    let blocked = match facts {
        None => Some("Deactivated or without a main character: in no smart group.".to_owned()),
        Some(f) => {
            let allowed = groups::allowed_states(&mut *conn, group).await?;
            if tether_core::groups::state_allowed(&allowed, StateId(f.state)) {
                None
            } else {
                let state = tether_db::states::all(&mut *conn)
                    .await?
                    .into_iter()
                    .find(|s| s.id.0 == f.state)
                    .map_or_else(|| "theirs".to_owned(), |s| s.name);
                Some(format!(
                    "Their state ({state}) isn't allowed in this group."
                ))
            }
        }
    };
    let mut filters: Vec<FilterCheck> = rules
        .iter()
        .map(|rule| {
            let unanswered = match &rule.filter {
                Filter::App {
                    plugin,
                    name,
                    config,
                    ..
                } => !known.contains(&tether_core::smart::app_key(plugin, name, config)),
                _ => false,
            };
            FilterCheck {
                text: names.describe(rule),
                passes: match facts {
                    Some(f) if !unanswered => Some(rule.passes(f)),
                    _ => None,
                },
            }
        })
        .collect();
    filters.extend(broken.iter().map(|_| FilterCheck {
        text: "a filter that no longer reads".to_owned(),
        passes: None,
    }));
    let passes = blocked.is_none() && filters.iter().all(|f| f.passes == Some(true));
    Ok(Explained {
        account,
        name,
        member,
        grace_since,
        blocked,
        filters,
        passes,
    })
}

/// What every smart change needs: the group, locked; the owner for a
/// Restricted one; its grants (it decides who gets them); and never a
/// compliance or Auto Group.
async fn may_change(
    tx: &mut sqlx::PgConnection,
    actor: AccountId,
    group: GroupId,
) -> Result<groups::Group, AppError> {
    if !groups::lock(&mut *tx, group, true).await? {
        return Err(AppError::not_found("No such group."));
    }
    let found = groups::get(&mut *tx, group)
        .await?
        .ok_or_else(|| AppError::not_found("No such group."))?;
    if found.compliance {
        return Err(crate::groups::managed_group());
    }
    if tether_db::autogroups::is_auto(&mut *tx, group).await? {
        return Err(crate::groups::auto_group());
    }
    if found.flags.restricted {
        crate::groups::restricted_owner(crate::groups::standing(&mut *tx, actor).await?.is_owner)?;
    }
    crate::groups::require_grants(&mut *tx, actor, group, "change who its filters let in").await?;
    Ok(found)
}

/// Makes a group smart (or changes its settings), or ordinary again.
pub async fn set_settings(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    settings: Option<Settings>,
) -> Result<(), AppError> {
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    if let Some(s) = settings {
        if !(0..=60).contains(&s.grace_days) {
            return Err(AppError::bad_request("A grace period is 0 to 60 days."));
        }
        // Leading a group is Group Management over it: never for everyone
        // who happens to pass a filter.
        if s.auto_join && !groups::leads(&mut *tx, group).await?.is_empty() {
            return Err(AppError::bad_request(
                "This group leads other groups, so it can't add members by itself. \
                 Remove it as their leader group first.",
            ));
        }
    }
    let (filters, _) = db::rules(&mut *tx, group).await?;
    db::set_settings(&mut *tx, group, settings).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.settings",
        Some(&format!("group:{}", group.0)),
        match settings {
            Some(s) => {
                json!({ "auto_join": s.auto_join, "grace_days": s.grace_days, "notify": s.notify })
            }
            None => json!({ "smart": false, "filters_removed": filters.len() }),
        },
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Checks a filter's ids: at most [`MAX_IDS`], states and groups that
/// exist, and never the group itself.
async fn check_filter(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    filter: &Filter,
) -> Result<(), AppError> {
    let too_many = || AppError::bad_request(format!("A filter lists at most {MAX_IDS}."));
    match filter {
        Filter::State { states } => {
            if states.len() > MAX_IDS {
                return Err(too_many());
            }
            let known: BTreeSet<i64> = tether_db::states::all(&mut *tx)
                .await?
                .into_iter()
                .map(|s| s.id.0)
                .collect();
            if states.iter().any(|s| !known.contains(s)) {
                return Err(AppError::bad_request("Choose states from the list."));
            }
        }
        Filter::Groups { groups: ids, .. } => {
            if ids.len() > MAX_IDS {
                return Err(too_many());
            }
            if ids.contains(&group.0) {
                return Err(AppError::bad_request(
                    "A group's filter can't name the group itself.",
                ));
            }
            for id in ids {
                if groups::get(&mut *tx, GroupId(*id)).await?.is_none() {
                    return Err(AppError::bad_request("Choose groups from the list."));
                }
            }
        }
        Filter::MainAffiliation { entities } | Filter::AnyAffiliation { entities } => {
            if entities.len() > MAX_IDS {
                return Err(too_many());
            }
        }
        Filter::CharacterAge { .. } | Filter::Compliant {} | Filter::App { .. } => {}
    }
    Ok(())
}

pub async fn add_filter(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    filter: Filter,
    reversed: bool,
) -> Result<(), AppError> {
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    if db::settings(&mut *tx, group).await?.is_none() {
        return Err(AppError::bad_request("Make it a smart group first."));
    }
    let (rules, broken) = db::rules(&mut *tx, group).await?;
    if rules.len() + broken.len() >= MAX_FILTERS {
        return Err(AppError::bad_request(format!(
            "A group has at most {MAX_FILTERS} filters."
        )));
    }
    check_filter(&mut tx, group, &filter).await?;
    let id = db::add_filter(&mut *tx, group, &filter, reversed).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.filter.add",
        Some(&format!("group:{}", group.0)),
        json!({ "filter": id, "kind": filter.kind(), "config": filter, "reversed": reversed }),
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn delete_filter(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    id: i64,
) -> Result<(), AppError> {
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    if !db::delete_filter(&mut *tx, group, id).await? {
        return Err(AppError::not_found("No such filter."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.filter.delete",
        Some(&format!("group:{}", group.0)),
        json!({ "filter": id }),
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
