use std::io::Write;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use serde_json::json;
use tether_core::tiers::Tier;
use tether_db::PgPool;
use tether_db::accounts;
use tether_db::audit::{self, Actor};
use tether_db::tiers as tier_db;
use tether_esi::Esi;
use tether_jobs::{JobId, JobState};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List accounts, or show one.
    Users {
        #[command(subcommand)]
        command: Option<UsersCommand>,
    },
    /// Show or change which alliances and corporations are Member or Allied.
    Tiers {
        #[command(subcommand)]
        command: Option<TiersCommand>,
    },
    /// Inspect the job queue, or retry a dead job.
    Jobs {
        #[command(subcommand)]
        command: Option<JobsCommand>,
        /// Only jobs in this state.
        #[arg(long)]
        state: Option<StateArg>,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Queue an affiliation and tier refresh for every account now.
    Sync,
}

#[derive(Debug, Subcommand)]
pub enum UsersCommand {
    /// Show one account by account id, character id or character name.
    Show { query: String },
}

#[derive(Debug, Subcommand)]
pub enum TiersCommand {
    /// Make an alliance or corporation (by EVE id) Member or Allied.
    Set { entity_id: i64, tier: TierArg },
    /// Remove the rule for an alliance or corporation.
    Remove { entity_id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// Put a dead job back in the queue.
    Retry { job_id: i64 },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TierArg {
    Member,
    Allied,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateArg {
    Queued,
    Running,
    Succeeded,
    Dead,
}

impl From<StateArg> for JobState {
    fn from(value: StateArg) -> Self {
        match value {
            StateArg::Queued => Self::Queued,
            StateArg::Running => Self::Running,
            StateArg::Succeeded => Self::Succeeded,
            StateArg::Dead => Self::Dead,
        }
    }
}

pub async fn run(
    command: Command,
    db: &PgPool,
    esi: &Esi,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match command {
        Command::Users { command: None } => list_users(db, out).await,
        Command::Users {
            command: Some(UsersCommand::Show { query }),
        } => show_user(db, &query, out).await,
        Command::Tiers { command: None } => list_tiers(db, out).await,
        Command::Tiers {
            command: Some(TiersCommand::Set { entity_id, tier }),
        } => set_tier(db, esi, entity_id, tier, out).await,
        Command::Tiers {
            command: Some(TiersCommand::Remove { entity_id }),
        } => remove_tier(db, entity_id, out).await,
        Command::Jobs {
            command: None,
            state,
            limit,
        } => list_jobs(db, state.map(Into::into), limit, out).await,
        Command::Jobs {
            command: Some(JobsCommand::Retry { job_id }),
            ..
        } => retry_job(db, job_id, out).await,
        Command::Sync => sync(db, out).await,
    }
}

async fn list_users(db: &PgPool, out: &mut dyn Write) -> anyhow::Result<()> {
    let users = accounts::list_summaries(db, 1000).await?;
    writeln!(
        out,
        "{:>6}  {:<28} {:<7} {:>5}  groups",
        "id", "main", "tier", "chars"
    )?;
    for u in &users {
        let name = if u.is_owner {
            format!("{} (owner)", u.main_name)
        } else {
            u.main_name.clone()
        };
        writeln!(
            out,
            "{:>6}  {:<28} {:<7} {:>5}  {}",
            u.id.0,
            name,
            u.tier,
            u.characters,
            u.groups.join(", ")
        )?;
    }
    writeln!(out, "{} account(s)", users.len())?;
    Ok(())
}

async fn show_user(db: &PgPool, query: &str, out: &mut dyn Write) -> anyhow::Result<()> {
    let Some(id) = accounts::find(db, query).await? else {
        bail!("no account matches {query:?}");
    };
    let account = accounts::get(db, id).await?.context("account vanished")?;
    let tier = tier_db::account_tier(db, id).await?.unwrap_or(Tier::Guest);
    let groups = tether_db::groups::names_for(db, id).await?;
    let permissions = tether_db::permissions::effective(db, id).await?;
    writeln!(
        out,
        "account {}{}",
        id.0,
        if account.is_owner { " (owner)" } else { "" }
    )?;
    writeln!(out, "tier: {tier}")?;
    writeln!(out, "characters:")?;
    for c in &account.characters {
        let main = if c.id == account.main.id {
            "  (main)"
        } else {
            ""
        };
        writeln!(out, "  {:>12}  {}{main}", c.id, c.name)?;
    }
    writeln!(out, "groups: {}", join_or_none(&groups))?;
    let permissions: Vec<String> = permissions.into_iter().collect();
    writeln!(out, "permissions: {}", join_or_none(&permissions))?;
    Ok(())
}

async fn list_tiers(db: &PgPool, out: &mut dyn Write) -> anyhow::Result<()> {
    let rules = tier_db::list_rules(db).await?;
    if rules.is_empty() {
        writeln!(
            out,
            "No tier rules: everyone is Guest. Add one with `tether tiers set <id> member`."
        )?;
    }
    for r in &rules {
        writeln!(
            out,
            "{:<7} {:<12} {:>12}  {}",
            r.tier,
            r.kind.as_str(),
            r.entity_id,
            r.name
        )?;
    }
    Ok(())
}

async fn set_tier(
    db: &PgPool,
    esi: &Esi,
    entity_id: i64,
    tier: TierArg,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let tier = match tier {
        TierArg::Member => Tier::Member,
        TierArg::Allied => Tier::Allied,
    };
    let entity = esi
        .names(&[entity_id])
        .await
        .context("looking the id up on ESI")?
        .into_iter()
        .find(|e| e.id == entity_id)
        .with_context(|| format!("ESI doesn't know id {entity_id}"))?;
    let Some(kind) = entity.kind else {
        bail!(
            "{entity_id} ({}) is not an alliance or corporation",
            entity.name
        );
    };
    let rule = tier_db::TierRule {
        entity_id,
        kind,
        tier,
        name: entity.name.clone(),
    };
    let mut tx = db.begin().await?;
    tier_db::set_rule(&mut *tx, &rule).await?;
    audit::record(
        &mut *tx,
        Actor::Cli,
        "tier.rule.set",
        Some(&format!("{}:{entity_id}", kind.as_str())),
        json!({ "name": entity.name, "tier": tier.as_str() }),
    )
    .await?;
    tether_web::tiers::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
    writeln!(
        out,
        "{} ({}) is now {tier}. Tiers will be re-evaluated shortly.",
        entity.name,
        kind.as_str()
    )?;
    Ok(())
}

async fn remove_tier(db: &PgPool, entity_id: i64, out: &mut dyn Write) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    if !tier_db::remove_rule(&mut *tx, entity_id).await? {
        bail!("no tier rule for {entity_id}");
    }
    audit::record(
        &mut *tx,
        Actor::Cli,
        "tier.rule.remove",
        Some(&entity_id.to_string()),
        json!({}),
    )
    .await?;
    tether_web::tiers::enqueue_evaluate_all(&mut *tx).await?;
    tx.commit().await?;
    writeln!(
        out,
        "Removed the rule for {entity_id}. Tiers will be re-evaluated shortly."
    )?;
    Ok(())
}

async fn list_jobs(
    db: &PgPool,
    state: Option<JobState>,
    limit: i64,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let counts = tether_jobs::counts(db).await?;
    let summary: Vec<String> = counts
        .iter()
        .map(|(s, n)| format!("{} {}", n, s.as_str()))
        .collect();
    writeln!(out, "{}", summary.join(", "))?;
    for j in tether_jobs::list(db, state, limit).await? {
        writeln!(
            out,
            "{:>8}  {:<10} {:<24} {}/{}  {}{}",
            j.id.0,
            j.state,
            j.kind,
            j.attempts,
            j.max_attempts,
            j.run_at.format("%Y-%m-%d %H:%M:%S"),
            j.last_error.map(|e| format!("  {e}")).unwrap_or_default()
        )?;
    }
    Ok(())
}

async fn retry_job(db: &PgPool, job_id: i64, out: &mut dyn Write) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    if !tether_jobs::retry(&mut *tx, JobId(job_id)).await? {
        bail!("job {job_id} is not dead (only dead jobs can be retried)");
    }
    audit::record(
        &mut *tx,
        Actor::Cli,
        "job.retry",
        Some(&format!("job:{job_id}")),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    writeln!(out, "Job {job_id} is queued again.")?;
    Ok(())
}

async fn sync(db: &PgPool, out: &mut dyn Write) -> anyhow::Result<()> {
    let ids = accounts::all_ids(db).await?;
    let mut tx = db.begin().await?;
    for id in &ids {
        let payload = json!({ "account_id": id.0 });
        tether_jobs::enqueue(
            &mut *tx,
            tether_jobs::NewJob::new(tether_web::tiers::REFRESH_ACCOUNT_JOB, payload),
        )
        .await?;
    }
    audit::record(
        &mut *tx,
        Actor::Cli,
        "sync.trigger",
        None,
        json!({ "accounts": ids.len() }),
    )
    .await?;
    tx.commit().await?;
    writeln!(
        out,
        "Queued an affiliation refresh for {} account(s).",
        ids.len()
    )?;
    Ok(())
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}
