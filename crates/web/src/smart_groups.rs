//! Secure Groups (allianceauth-secure-groups, AA's "Smart Groups"): groups
//! whose members Tether keeps by filters. An hourly sweep (and one after
//! any change) adds everyone who passes to auto groups, and removes members
//! who stop passing, at once or when a failing filter's grace period ends.
//! Joining or requesting a smart group needs passing it (AA's views don't
//! check; Tether does). Everything is audited and in the group's Audit Log.
//!
//! As AA: `securegroups.access_sec_group` opens the Secure Groups page,
//! where pilots check themselves and join; `securegroups.audit_sec_group`
//! with Group Management over a group opens its audit. Setting a group up
//! is `admin.groups` (AA: the Django admin), and changing a smart group is
//! changing who is in it, so it needs what adding members would: the
//! group's grants, and the owner for Restricted groups.
//!
//! AA posts each run's summary to a group's Discord webhooks; Tether has no
//! webhooks (every destination is on the allow list), so the bot posts it
//! to a ping channel the admin chooses, through the job queue.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tether_core::crypto::EncryptionKey;
use tether_core::smart::{Facts, Filter, Operator, Rule, Service, Term, failing};
use tether_core::states::StateId;
use tether_db::PgPool;
use tether_db::accounts::AccountId;
use tether_db::audit::{self, Actor};
use tether_db::groups::{self, GroupId, RequestType};
use tether_db::smart_groups::{self as db, Settings};
use tether_discord::{Discord, Mention};
use tether_esi::{Esi, Priority};
use tether_jobs::schedule::ScheduleSpec;
use tether_jobs::{JobError, NewJob, Registry};

use crate::error::AppError;

pub const SWEEP_JOB: &str = "smart_groups.sweep";
/// Posts a run's summary to the group's update channel.
pub const POST_JOB: &str = "smart_groups.post_update";
const EVERY: Duration = Duration::from_secs(60 * 60);
/// Birthdays asked for per sweep, for the character age filter.
const BIRTHDAYS_PER_SWEEP: i64 = 200;
/// Most filters on one group (counting each inside an expression), and ids
/// in one filter.
pub const MAX_FILTERS: usize = 20;
pub const MAX_IDS: usize = 50;
/// A filter's grace period, as AA's default, and its most.
pub const DEFAULT_GRACE_DAYS: i32 = 5;
pub const MAX_GRACE_DAYS: i32 = 60;

pub fn schedules() -> Vec<ScheduleSpec> {
    vec![ScheduleSpec::new(SWEEP_JOB, SWEEP_JOB, EVERY)]
}

#[derive(Debug, Deserialize)]
struct PostJob {
    group_id: i64,
    channel_id: i64,
    content: String,
    nonce: String,
}

pub fn register_jobs(
    registry: &mut Registry,
    db: PgPool,
    esi: Esi,
    key: EncryptionKey,
    discord: Arc<Discord>,
) {
    let sweep_db = db.clone();
    registry.register(SWEEP_JOB, move |_job| {
        let (db, esi) = (sweep_db.clone(), esi.clone());
        async move {
            let changed = sweep(&db, &esi).await.map_err(JobError::retry)?;
            tracing::info!(changed, "Secure Groups swept");
            Ok(())
        }
    });
    registry.register(POST_JOB, move |job| {
        let (db, key, discord) = (db.clone(), key.clone(), discord.clone());
        async move {
            let payload: PostJob =
                serde_json::from_value(job.payload).map_err(JobError::permanent)?;
            post_update(&db, &key, &discord, &payload).await
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

/// Names for filter descriptions: states, groups, corporations, alliances
/// and factions.
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
            for leaf in rule.filter.leaves() {
                match leaf {
                    Filter::MainAffiliation { entities: e }
                    | Filter::AnyAffiliation { entities: e, .. } => entities.extend(e),
                    Filter::Faction { factions } => entities.extend(factions),
                    _ => {}
                }
                entities.extend(leaf.exempt());
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

    /// "main in Pandemic Horde", "not a character in Spies, unless the
    /// main is in Friends", "(state is Member OR main at least 30 days
    /// old)".
    pub fn describe_filter(&self, filter: &Filter, reversed: bool) -> String {
        let text = match filter {
            Filter::State { states } => format!("state is {}", Self::list(&self.states, states)),
            Filter::MainAffiliation { entities } => {
                format!("main in {}", Self::list(&self.entities, entities))
            }
            Filter::AnyAffiliation { entities, .. } => {
                format!("a character in {}", Self::list(&self.entities, entities))
            }
            Filter::CharacterAge { days } => format!("main at least {days} days old"),
            Filter::Groups { groups, all, .. } => format!(
                "in {} of: {}",
                if *all { "all" } else { "any" },
                self.group_list(groups)
            ),
            Filter::Compliant {} => "compliant".to_owned(),
            Filter::Faction { factions } => format!(
                "a character in the militia of {}",
                Self::list(&self.entities, factions)
            ),
            Filter::Service {
                service: Service::Discord,
            } => "linked to Discord".to_owned(),
            Filter::Expression {
                operator,
                negate,
                first,
                second,
            } => format!(
                "{}({} {} {})",
                if *negate { "not " } else { "" },
                self.describe_filter(&first.filter, first.reversed),
                operator.as_str(),
                self.describe_filter(&second.filter, second.reversed)
            ),
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
        let text = if reversed {
            format!("not {text}")
        } else {
            text
        };
        match filter.exempt() {
            [] => text,
            exempt => format!(
                "{text}, unless the main is in {}",
                Self::list(&self.entities, exempt)
            ),
        }
    }

    pub fn describe(&self, rule: &Rule) -> String {
        self.describe_filter(&rule.filter, rule.reversed)
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

/// The app filters in a filter (on its own or inside an expression), by
/// key.
fn app_keys(filter: &Filter) -> Vec<String> {
    filter
        .leaves()
        .into_iter()
        .filter_map(|leaf| match leaf {
            Filter::App {
                plugin,
                name,
                config,
                ..
            } => Some(tether_core::smart::app_key(plugin, name, config)),
            _ => None,
        })
        .collect()
}

fn uses_app(rules: &[Rule]) -> bool {
    rules.iter().any(|r| !app_keys(&r.filter).is_empty())
}

/// Whether a filter needs an app that hasn't answered lately.
fn unanswered(filter: &Filter, known: &BTreeSet<String>) -> bool {
    app_keys(filter).iter().any(|key| !known.contains(key))
}

/// An app filter with no fresh values (the app is gone, or hasn't reported
/// lately): the group can't be judged, so it fails closed.
fn unknown_app(rules: &[Rule], known: &BTreeSet<String>) -> bool {
    rules.iter().any(|r| unanswered(&r.filter, known))
}

/// Refuses an account a smart group it doesn't pass, naming why (Internal
/// groups unnamed). Ordinary groups, and smart groups switched off, pass.
/// Runs in the caller's transaction. Callers decide first that the group
/// is one the account may see and join, so this never reveals a group.
pub async fn check(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    account: AccountId,
) -> Result<(), AppError> {
    if db::active(&mut *tx, group).await?.is_none() {
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

/// Brings every smart group that's switched on and included in updates in
/// line with its filters. Returns how many memberships changed.
pub async fn sweep(db: &PgPool, esi: &Esi) -> Result<usize, sqlx::Error> {
    let groups: Vec<GroupId> = db::all(db)
        .await?
        .into_iter()
        .filter(|(_, s)| s.enabled && s.include_in_updates)
        .map(|(g, _)| g)
        .collect();
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
    for group in groups {
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
    /// Left alone: gone, not smart (any more), switched off, kept another
    /// way, or with a filter that can't be judged now.
    Skipped,
}

/// A run's counts, for AA's summary.
#[derive(Debug, Default)]
struct Tally {
    checked: usize,
    added: usize,
    removed: usize,
    pending: usize,
}

/// AA's summary of a run: "**Group**: Checked 10 Members, Approved 9,
/// Added 1, Removed 2 (Pending Removals 1)", then the extra message. It
/// mentions nobody.
fn summary(group: &str, approved: usize, t: &Tally, extra: &str) -> String {
    let mut text = format!(
        "**{}**: Checked {} Members, Approved {approved}, Added {}, Removed {} (Pending Removals {})",
        crate::pings::defuse(&group.replace('*', "")),
        t.checked,
        t.added,
        t.removed,
        t.pending,
    );
    let extra = extra.trim();
    if !extra.is_empty() {
        text.push('\n');
        text.push_str(&crate::pings::defuse(extra));
    }
    text
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
    let Some(settings) = db::active(&mut *tx, group).await? else {
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
    let mut tally = Tally::default();
    // `None`: can't be in the group at all (no main, deactivated, or a
    // state it doesn't allow); else the filters failed.
    let judge_account = |account: &AccountId| -> Option<Vec<&Rule>> {
        let f = facts.get(account)?;
        if !tether_core::groups::state_allowed(&allowed, StateId(f.state)) {
            return None;
        }
        Some(failing(&rules, f))
    };
    let none = HashMap::new();
    for account in &members {
        tally.checked += 1;
        let graced = grace.get(account).unwrap_or(&none);
        let (failed, eligible) = match judge_account(account) {
            Some(failed) if failed.is_empty() => {
                if db::end_grace(&mut *tx, group, *account, &[]).await? {
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
            Some(failed) => (failed, true),
            None => (Vec::new(), false),
        };
        let why = if eligible {
            failed
                .iter()
                .map(|r| names.describe(r))
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            "not eligible for this group".to_owned()
        };
        // Grace on filters that pass again is over.
        let failing_ids: Vec<i64> = failed.iter().map(|r| r.id).collect();
        db::end_grace(&mut *tx, group, *account, &failing_ids).await?;
        // Ineligible accounts (deactivated, without a main, a state the
        // group doesn't allow) leave at once. A failing filter removes at
        // once too, unless the group can grace and the filter has a grace
        // period (AA's), which runs from when it first failed.
        let mut remove_now = !eligible;
        let mut started = Vec::new();
        for rule in &failed {
            match graced.get(&rule.id) {
                Some(expires) if *expires <= now => remove_now = true,
                Some(_) => {}
                None if settings.can_grace && rule.grace_days > 0 => {
                    started.push((
                        rule.id,
                        now + chrono::Duration::days(i64::from(rule.grace_days)),
                    ));
                }
                None => remove_now = true,
            }
        }
        if !remove_now {
            tally.pending += 1;
            if !started.is_empty() {
                for (rule, expires) in &started {
                    db::start_grace(&mut *tx, group, *account, *rule, *expires).await?;
                }
                // They leave when the first grace period ends.
                let ends = graced
                    .iter()
                    .filter(|(id, _)| failing_ids.contains(id))
                    .map(|(_, e)| *e)
                    .chain(started.iter().map(|(_, e)| *e))
                    .min()
                    .unwrap_or(now);
                if settings.notify_on_grace {
                    let date = ends.format("%Y-%m-%d").to_string();
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
                    json!({
                        "account_id": account.0,
                        "failing": why,
                        "filters": started.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                        "ends": ends,
                    }),
                )
                .await?;
            }
            continue;
        }
        groups::remove_member(&mut *tx, group, *account).await?;
        db::end_grace(&mut *tx, group, *account, &[]).await?;
        groups::log(&mut *tx, group, RequestType::Removed, true, *account, None).await?;
        audit::record(
            &mut *tx,
            by,
            "group.member.remove",
            Some(&format!("group:{}", group.0)),
            json!({ "account_id": account.0, "reason": "smart group", "failing": why }),
        )
        .await?;
        if settings.notify_on_remove && eligible {
            crate::notifications::smart_failing(&mut tx, *account, &found.name, &why, None).await?;
        }
        tally.removed += 1;
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
                || !matches!(judge_account(account), Some(failed) if failed.is_empty())
            {
                continue;
            }
            tally.checked += 1;
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
            if settings.notify_on_add {
                crate::notifications::smart_added(&mut tx, *account, &found.name).await?;
            }
            tally.added += 1;
        }
    }
    sqlx::query!(
        "UPDATE core.smart_groups SET swept_at = now() WHERE group_id = $1",
        group.0
    )
    .execute(&mut *tx)
    .await?;
    let changed = tally.added + tally.removed;
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
    // AA's group update webhook: the run's summary, every run.
    if let Some(channel) = settings.update_channel {
        let approved = db::member_ids(&mut *tx, group).await?.len();
        let nonce = match tether_core::new_token() {
            Ok(token) => format!("sg-{}", &token.expose()[..20]),
            Err(err) => {
                tracing::warn!(error = %err, "no random nonce for the update post");
                format!("sg-{}-{}", group.0, now.timestamp())
            }
        };
        tether_jobs::enqueue(
            &mut *tx,
            NewJob::new(
                POST_JOB,
                json!({
                    "group_id": group.0,
                    "channel_id": channel,
                    "content": summary(&found.name, approved, &tally, &settings.update_message),
                    "nonce": nonce,
                }),
            ),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Swept::Changed(changed))
}

/// Posts a run's summary: only to a channel that's still a ping channel on
/// the configured server, never mentioning anyone.
async fn post_update(
    db: &PgPool,
    key: &EncryptionKey,
    discord: &Discord,
    job: &PostJob,
) -> Result<(), JobError> {
    let Some(config) = tether_discord::store::load(db, key)
        .await
        .map_err(JobError::retry)?
    else {
        return Err(JobError::permanent("Discord isn't set up"));
    };
    let guild = i64::try_from(config.guild_id).map_err(JobError::permanent)?;
    if !tether_db::pings::is_channel(db, job.channel_id, guild)
        .await
        .map_err(JobError::retry)?
    {
        return Err(JobError::permanent(
            "that channel is no longer a ping channel; not posted",
        ));
    }
    let channel = u64::try_from(job.channel_id).map_err(JobError::permanent)?;
    match discord
        .send_message(
            &config,
            channel,
            &job.content,
            None,
            Mention::None,
            &job.nonce,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(group = job.group_id, "Secure Groups update posted");
            Ok(())
        }
        Err(err) if err.is_transient() => Err(JobError::retry(err)),
        Err(err) => Err(JobError::permanent(err)),
    }
}

/// Check now waits this long after the group was last judged (by the
/// sweep or a Check now): each reads every account's facts.
const CHECK_GAP: chrono::Duration = chrono::Duration::seconds(15);

/// Check now (aa-securegroups' manual refresh): brings one smart group in
/// line with its filters at once, as the hourly sweep does, audited as the
/// actor's (the check and every change it makes, in one transaction).
/// Returns how many memberships changed. Birthdays ESI hasn't told Tether
/// yet wait for the sweep (a click never waits on a batch of ESI calls).
/// Nothing about the filters changes, so it needs only what opens the page
/// it's on (`admin.groups`, or the group's audit); callers check.
pub async fn check_now(db: &PgPool, actor: AccountId, group: GroupId) -> Result<usize, AppError> {
    let Some(settings) = db::settings(db, group).await? else {
        return Err(AppError::bad_request(
            "Only a smart group has filters to check.",
        ));
    };
    if !settings.enabled {
        return Err(AppError::bad_request(
            "This smart group is switched off: switch it on to check it.",
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
/// account.
pub struct Explained {
    pub account: AccountId,
    /// The account's main.
    pub name: String,
    pub member: bool,
    /// In a grace period until then (the first to end).
    pub grace_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Why the account can't be in the group whatever the filters say.
    pub blocked: Option<String>,
    pub filters: Vec<FilterCheck>,
    /// Blocked by nothing, and every filter passes.
    pub passes: bool,
}

/// How each filter judges one account's facts (no facts: none can say).
fn checks(
    rules: &[Rule],
    broken: &[i64],
    names: &Names,
    known: &BTreeSet<String>,
    facts: Option<&Facts>,
) -> Vec<FilterCheck> {
    let mut filters: Vec<FilterCheck> = rules
        .iter()
        .map(|rule| FilterCheck {
            text: names.describe(rule),
            passes: match facts {
                Some(f) if !unanswered(&rule.filter, known) => Some(rule.passes(f)),
                _ => None,
            },
        })
        .collect();
    filters.extend(broken.iter().map(|_| FilterCheck {
        text: "a filter that no longer reads".to_owned(),
        passes: None,
    }));
    filters
}

/// Why an account can't be in the group whatever its filters say.
async fn blocked(
    conn: &mut sqlx::PgConnection,
    group: GroupId,
    facts: Option<&Facts>,
) -> Result<Option<String>, AppError> {
    Ok(match facts {
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
    })
}

/// When an account's first grace period on a group ends.
fn grace_until(
    grace: &HashMap<AccountId, HashMap<i64, chrono::DateTime<chrono::Utc>>>,
    account: AccountId,
) -> Option<chrono::DateTime<chrono::Utc>> {
    grace.get(&account).and_then(|g| g.values().min().copied())
}

/// How `group`'s filters judge `account` now, for `actor`, audited (it
/// shows an account's main, standing and filter results). Members and
/// those in their grace period are anyone's who may open the page to
/// check; anyone else needs `anyone` (the checker holds `admin.users`, who
/// can look up any account already). Changes nothing else.
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
    let grace_until = grace_until(&db::grace(db, group).await?, account);
    if !member && grace_until.is_none() && !anyone {
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
    let blocked = blocked(&mut conn, group, facts).await?;
    let filters = checks(&rules, &broken, &names, &known, facts);
    let passes = blocked.is_none() && filters.iter().all(|f| f.passes == Some(true));
    Ok(Explained {
        account,
        name,
        member,
        grace_until,
        blocked,
        filters,
        passes,
    })
}

// ---- Secure Groups (access_sec_group) --------------------------------------

/// A smart group on a pilot's Secure Groups page, with how its filters
/// judge them.
pub struct Offered {
    pub group: groups::Group,
    pub member: bool,
    /// `join` or `leave` while a request waits.
    pub pending: Option<&'static str>,
    pub filters: Vec<FilterCheck>,
    pub blocked: Option<String>,
    pub passes: bool,
    /// Pending removal: when their first grace period ends.
    pub grace_until: Option<chrono::DateTime<chrono::Utc>>,
}

/// AA's Secure Groups page: the smart groups that are switched on, not
/// automatic, open to the account's state (or every state; members see
/// theirs whatever), and not Internal, each checked against the account
/// (AA's Check; it's their own standing, so not audited).
pub async fn offered(db: &PgPool, account: AccountId) -> Result<Vec<Offered>, AppError> {
    let (member_of, pending) = groups::memberships(db, account).await?;
    let mut conn = db.acquire().await?;
    let mut facts = db::facts(&mut *conn, Some(&[account])).await?;
    let known = fill_app(&mut conn, &mut facts, Some(account)).await?;
    let mine = facts.get(&account);
    let state = tether_db::states::account_state(&mut *conn, account)
        .await?
        .map(|s| s.id);
    let mut out = Vec::new();
    for (group, settings) in db::all(&mut *conn).await? {
        if !settings.enabled || settings.auto_join {
            continue;
        }
        let Some(found) = groups::get(&mut *conn, group).await? else {
            continue;
        };
        if found.flags.internal || found.compliance {
            continue;
        }
        let member = member_of.contains(&group);
        let allowed = groups::allowed_states(&mut *conn, group).await?;
        if !member && !state.is_some_and(|s| tether_core::groups::state_allowed(&allowed, s)) {
            continue;
        }
        let (rules, broken) = db::rules(&mut *conn, group).await?;
        let names = Names::load(&mut conn, &rules, true).await?;
        let blocked = blocked(&mut conn, group, mine).await?;
        let filters = checks(&rules, &broken, &names, &known, mine);
        let passes = blocked.is_none() && filters.iter().all(|f| f.passes == Some(true));
        out.push(Offered {
            member,
            pending: pending
                .iter()
                .find(|(g, _)| *g == group)
                .map(|(_, leave)| if *leave { "leave" } else { "join" }),
            filters,
            blocked,
            passes,
            grace_until: grace_until(&db::grace(&mut *conn, group).await?, account),
            group: found,
        });
    }
    out.sort_by_key(|o| o.group.name.to_lowercase());
    Ok(out)
}

/// Whether the group is on the account's Secure Groups page.
pub async fn is_offered(db: &PgPool, account: AccountId, group: GroupId) -> Result<bool, AppError> {
    Ok(offered(db, account)
        .await?
        .iter()
        .any(|o| o.group.id == group))
}

// ---- Secure Group Audit (audit_sec_group + Group Management) ---------------

/// One member of a smart group against each filter.
pub struct AuditRow {
    pub account_id: i64,
    pub main_id: i64,
    pub main_name: String,
    pub blocked: Option<String>,
    pub checks: Vec<Option<bool>>,
    pub grace_until: Option<chrono::DateTime<chrono::Utc>>,
}

/// aa-securegroups' audit: the group's filters, and every member against
/// each of them. Audited (it shows every member's standing). Callers
/// check the actor may audit the group.
pub async fn audit_group(
    db: &PgPool,
    actor: AccountId,
    group: GroupId,
) -> Result<(Vec<String>, Vec<AuditRow>), AppError> {
    // Leaders don't learn other groups' names: Internal ones stay
    // unnamed unless the actor sets groups up anyway.
    let public = !tether_db::permissions::effective(db, actor)
        .await?
        .contains(tether_core::permissions::ADMIN_GROUPS);
    if db::active(db, group).await?.is_none() {
        return Err(AppError::not_found("No such smart group."));
    }
    audit::record(
        db,
        Actor::Account(actor),
        "smart_group.audit",
        Some(&format!("group:{}", group.0)),
        json!({}),
    )
    .await?;
    let members = groups::members(db, group).await?;
    let ids: Vec<AccountId> = members.iter().map(|m| AccountId(m.account_id)).collect();
    let mut conn = db.acquire().await?;
    let (rules, broken) = db::rules(&mut *conn, group).await?;
    let names = Names::load(&mut conn, &rules, public).await?;
    let mut facts = db::facts(&mut *conn, Some(&ids)).await?;
    let known = fill_app(&mut conn, &mut facts, None).await?;
    let grace = db::grace(&mut *conn, group).await?;
    let headers = checks(&rules, &broken, &names, &known, None)
        .into_iter()
        .map(|c| c.text)
        .collect();
    let mut rows = Vec::new();
    for m in members {
        let account = AccountId(m.account_id);
        let f = facts.get(&account);
        rows.push(AuditRow {
            blocked: blocked(&mut conn, group, f).await?,
            checks: checks(&rules, &broken, &names, &known, f)
                .into_iter()
                .map(|c| c.passes)
                .collect(),
            grace_until: grace_until(&grace, account),
            account_id: m.account_id,
            main_id: m.main_id,
            main_name: m.main_name,
        });
    }
    Ok((headers, rows))
}

// ---- set-up (callers check admin.groups) -----------------------------------

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
    if let Some(s) = &settings {
        // Leading a group is Group Management over it: never for everyone
        // who happens to pass a filter.
        if s.auto_join && !groups::leads(&mut *tx, group).await?.is_empty() {
            return Err(AppError::bad_request(
                "This group leads other groups, so it can't add members by itself. \
                 Remove it as their leader group first.",
            ));
        }
        if s.update_message.chars().count() > 500 {
            return Err(AppError::bad_request(
                "The update message is at most 500 characters.",
            ));
        }
        if let Some(channel) = s.update_channel {
            let guild: Option<i64> =
                tether_db::settings::get_string(&mut *tx, tether_db::settings::DISCORD_GUILD_ID)
                    .await?
                    .and_then(|g| g.parse().ok());
            let ping_channel = match guild {
                Some(guild) => tether_db::pings::is_channel(&mut *tx, channel, guild).await?,
                None => false,
            };
            if !ping_channel {
                return Err(AppError::bad_request(
                    "Choose one of the ping channels (Discord page) for updates.",
                ));
            }
        }
    }
    let (filters, _) = db::rules(&mut *tx, group).await?;
    db::set_settings(&mut *tx, group, settings.as_ref()).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.settings",
        Some(&format!("group:{}", group.0)),
        match &settings {
            Some(s) => json!({
                "auto_join": s.auto_join,
                "enabled": s.enabled,
                "include_in_updates": s.include_in_updates,
                "can_grace": s.can_grace,
                "notify_on_add": s.notify_on_add,
                "notify_on_remove": s.notify_on_remove,
                "notify_on_grace": s.notify_on_grace,
                "update_channel": s.update_channel,
            }),
            None => json!({ "smart": false, "filters_removed": filters.len() }),
        },
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Checks a filter's ids, all the way down: at most [`MAX_IDS`] each,
/// states and groups that exist, never the group itself. Returns how many
/// filters it's made of.
async fn check_filter(
    tx: &mut sqlx::PgConnection,
    group: GroupId,
    filter: &Filter,
) -> Result<usize, AppError> {
    let too_many = || AppError::bad_request(format!("A filter lists at most {MAX_IDS}."));
    let leaves = filter.leaves();
    for leaf in &leaves {
        if leaf.exempt().len() > MAX_IDS {
            return Err(too_many());
        }
        match leaf {
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
            Filter::MainAffiliation { entities } | Filter::AnyAffiliation { entities, .. } => {
                if entities.len() > MAX_IDS {
                    return Err(too_many());
                }
            }
            Filter::Faction { factions } => {
                if factions.len() > MAX_IDS {
                    return Err(too_many());
                }
            }
            Filter::CharacterAge { .. }
            | Filter::Compliant {}
            | Filter::Service { .. }
            | Filter::App { .. }
            | Filter::Expression { .. } => {}
        }
    }
    Ok(leaves.len())
}

fn check_grace(days: i32) -> Result<(), AppError> {
    if (0..=MAX_GRACE_DAYS).contains(&days) {
        Ok(())
    } else {
        Err(AppError::bad_request(format!(
            "A grace period is 0 to {MAX_GRACE_DAYS} days."
        )))
    }
}

/// How many filters a group's rules are made of.
fn size(rules: &[Rule], broken: &[i64]) -> usize {
    rules.iter().map(|r| r.filter.leaves().len()).sum::<usize>() + broken.len()
}

pub async fn add_filter(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    filter: Filter,
    reversed: bool,
    grace_days: i32,
) -> Result<(), AppError> {
    check_grace(grace_days)?;
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    if db::settings(&mut *tx, group).await?.is_none() {
        return Err(AppError::bad_request("Make it a smart group first."));
    }
    let (rules, broken) = db::rules(&mut *tx, group).await?;
    let adding = check_filter(&mut tx, group, &filter).await?;
    if size(&rules, &broken) + adding > MAX_FILTERS {
        return Err(AppError::bad_request(format!(
            "A group has at most {MAX_FILTERS} filters."
        )));
    }
    let id = db::add_filter(&mut *tx, group, &filter, reversed, grace_days).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.filter.add",
        Some(&format!("group:{}", group.0)),
        json!({
            "filter": id,
            "kind": filter.kind(),
            "config": filter,
            "reversed": reversed,
            "grace_days": grace_days,
        }),
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Changes one filter's grace period.
pub async fn set_filter_grace(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    id: i64,
    grace_days: i32,
) -> Result<(), AppError> {
    check_grace(grace_days)?;
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    if !db::set_filter_grace(&mut *tx, group, id, grace_days).await? {
        return Err(AppError::not_found("No such filter."));
    }
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.filter.grace",
        Some(&format!("group:{}", group.0)),
        json!({ "filter": id, "grace_days": grace_days }),
    )
    .await?;
    queue_sweep(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Combines two of a group's filters into one expression (AA's filter
/// expression): `first operator second`, maybe negated. The expression
/// takes the first's grace period.
pub async fn combine(
    db_pool: &PgPool,
    actor: AccountId,
    group: GroupId,
    first: i64,
    second: i64,
    operator: Operator,
    negate: bool,
) -> Result<(), AppError> {
    if first == second {
        return Err(AppError::bad_request("Choose two different filters."));
    }
    let mut tx = db_pool.begin().await?;
    may_change(&mut tx, actor, group).await?;
    let (rules, _) = db::rules(&mut *tx, group).await?;
    let find = |id: i64| {
        rules
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| AppError::not_found("No such filter."))
    };
    let (a, b) = (find(first)?, find(second)?);
    let filter = Filter::Expression {
        operator,
        negate,
        first: Box::new(Term {
            filter: a.filter.clone(),
            reversed: a.reversed,
        }),
        second: Box::new(Term {
            filter: b.filter.clone(),
            reversed: b.reversed,
        }),
    };
    db::delete_filter(&mut *tx, group, first).await?;
    db::delete_filter(&mut *tx, group, second).await?;
    let id = db::add_filter(&mut *tx, group, &filter, false, a.grace_days).await?;
    audit::record(
        &mut *tx,
        Actor::Account(actor),
        "smart_group.filter.combine",
        Some(&format!("group:{}", group.0)),
        json!({ "filter": id, "from": [first, second], "config": filter }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_read_as_aas_and_mention_nobody() {
        let tally = Tally {
            checked: 10,
            added: 1,
            removed: 2,
            pending: 1,
        };
        assert_eq!(
            summary("Caps", 9, &tally, " @everyone look "),
            "**Caps**: Checked 10 Members, Approved 9, Added 1, Removed 2 (Pending Removals 1)\n\
             @\u{200B}everyone look"
        );
    }
}
