use std::io::Write;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use serde_json::json;
use tether_core::states::State;
use tether_db::PgPool;
use tether_db::accounts;
use tether_db::audit::{self, Actor};
use tether_db::states as state_db;
use tether_esi::Esi;
use tether_jobs::{JobId, JobState};
use tether_web::state_admin::Change;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List accounts, or show one.
    Users {
        #[command(subcommand)]
        command: Option<UsersCommand>,
    },
    /// Show the access states, or change what one covers.
    States {
        #[command(subcommand)]
        command: Option<StatesCommand>,
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
    /// Queue an affiliation sync for every character now.
    Sync,
}

#[derive(Debug, Subcommand)]
pub enum UsersCommand {
    /// Show one account by account id, character id or character name.
    Show { query: String },
}

#[derive(Debug, Subcommand)]
pub enum StatesCommand {
    /// Make a state cover an alliance, corporation or character (by EVE id),
    /// e.g. `tether states add Member 99000001`.
    Add { state: String, entity_id: i64 },
    /// Stop a state covering an alliance, corporation or character.
    Remove { state: String, entity_id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// Put a dead job back in the queue.
    Retry { job_id: i64 },
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
        Command::States { command: None } => list_states(db, out).await,
        Command::States {
            command: Some(StatesCommand::Add { state, entity_id }),
        } => add_to_state(db, esi, &state, entity_id, out).await,
        Command::States {
            command: Some(StatesCommand::Remove { state, entity_id }),
        } => remove_from_state(db, esi, &state, entity_id, out).await,
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
        "{:>6}  {:<28} {:<12} {:>5}  groups",
        "id", "main", "state", "chars"
    )?;
    for u in &users {
        let name = if u.is_owner {
            format!("{} (owner)", u.main_name)
        } else {
            u.main_name.clone()
        };
        writeln!(
            out,
            "{:>6}  {:<28} {:<12} {:>5}  {}",
            u.id.0,
            name,
            u.state,
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
    let state = state_db::account_state(db, id)
        .await?
        .map_or_else(|| "Guest".to_owned(), |s| s.name);
    let groups = tether_db::groups::names_for(db, id).await?;
    let permissions = tether_db::permissions::effective(db, id).await?;
    writeln!(
        out,
        "account {}{}",
        id.0,
        if account.is_owner { " (owner)" } else { "" }
    )?;
    writeln!(out, "state: {state}")?;
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

async fn list_states(db: &PgPool, out: &mut dyn Write) -> anyhow::Result<()> {
    let states = state_db::list(db).await?;
    let covered = state_db::covered(db).await?;
    let counts = state_db::counts(db).await?;
    writeln!(
        out,
        "Checked from the top; the first state covering a main wins. Everyone else is Guest."
    )?;
    for s in &states {
        writeln!(
            out,
            "{}  ({} account(s))",
            s.name,
            counts.get(&s.id).copied().unwrap_or(0)
        )?;
        for c in covered.iter().filter(|c| c.state == s.id) {
            writeln!(
                out,
                "  {:<12} {:>12}  {}",
                c.kind.as_str(),
                c.entity_id,
                c.name
            )?;
        }
    }
    Ok(())
}

async fn find_state(db: &PgPool, name: &str) -> anyhow::Result<State> {
    state_db::by_name(db, name)
        .await?
        .with_context(|| format!("no state is called {name:?}; see `tether states`"))
}

/// Through the same code as the admin pages and API: checks, audit and
/// re-evaluation all happen there.
async fn apply_change(db: &PgPool, esi: &Esi, change: Change) -> anyhow::Result<()> {
    tether_web::state_admin::apply(db, esi, Actor::Cli, &change)
        .await
        .map_err(|err| anyhow::anyhow!("{}", err.message()))?;
    Ok(())
}

async fn add_to_state(
    db: &PgPool,
    esi: &Esi,
    state_name: &str,
    entity_id: i64,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let target = find_state(db, state_name).await?;
    let change = Change::Add {
        state: target.id,
        entity_id,
    };
    apply_change(db, esi, change).await?;
    let covered = state_db::covered(db).await?;
    let added = covered
        .iter()
        .find(|c| c.state == target.id && c.entity_id == entity_id)
        .context("the new entry vanished")?;
    writeln!(
        out,
        "{} now covers {} ({}). States will be re-evaluated shortly.",
        target.name,
        added.name,
        added.kind.as_str()
    )?;
    Ok(())
}

async fn remove_from_state(
    db: &PgPool,
    esi: &Esi,
    state_name: &str,
    entity_id: i64,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let target = find_state(db, state_name).await?;
    let change = Change::Remove {
        state: target.id,
        entity_id,
    };
    apply_change(db, esi, change).await?;
    writeln!(
        out,
        "{} no longer covers {entity_id}. States will be re-evaluated shortly.",
        target.name
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
    let mut tx = db.begin().await?;
    let job = tether_jobs::enqueue(
        &mut *tx,
        tether_jobs::NewJob::new(tether_web::sync::AFFILIATION_SYNC_JOB, json!({})),
    )
    .await?;
    audit::record(
        &mut *tx,
        Actor::Cli,
        "sync.trigger",
        Some(&format!("job:{job}")),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    writeln!(
        out,
        "Queued an affiliation sync for every character (job {job})."
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
