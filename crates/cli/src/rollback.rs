//! `tether rollback` (N14): put core, or one app's data, back as it was in
//! a snapshot or nightly backup.
//!
//! With the server stopped: `docker compose stop app`, then
//! `docker compose run --rm app rollback`. By default it restores the
//! newest snapshot taken before migrations; it shows when that was, warns
//! that everything since is lost, and asks to confirm.

use std::io::{BufRead, Write};

use anyhow::{Context, bail};
use chrono::Utc;
use tether_db::PgPool;
use tether_db::audit::Actor;
use tether_snapshots::{Kind, Reason, Snapshot, Snapshots};

#[derive(Debug, Clone, clap::Args)]
pub struct Args {
    /// List every snapshot and backup, and change nothing.
    #[arg(long)]
    pub list: bool,
    /// Roll back this app's data (by its id) instead of core.
    #[arg(long, value_name = "APP_ID")]
    pub plugin: Option<String>,
    /// A snapshot or backup by file name, from `--list`. Default: the
    /// newest snapshot taken before migrations.
    #[arg(long, value_name = "NAME", conflicts_with = "plugin")]
    pub snapshot: Option<String>,
    /// Don't ask to confirm (for scripts).
    #[arg(long)]
    pub yes: bool,
}

pub async fn run(
    args: Args,
    db: &PgPool,
    snapshots: &Snapshots,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let all = tether_snapshots::list(snapshots.dir()).await?;
    if args.list {
        return list(&all, out);
    }
    refuse_while_running(db).await?;
    let snapshot = pick(&all, &args)?;
    // Refuse what can't work before asking anything.
    snapshots.check(db, snapshot).await?;

    let header = &snapshot.header;
    writeln!(out, "Snapshot: {}", snapshot.name)?;
    writeln!(out, "Data:     {}", header.kind)?;
    writeln!(
        out,
        "Taken:    {} UTC ({}), {}, by Tether {}",
        header.taken_at.format("%Y-%m-%d %H:%M:%S"),
        ago(header.taken_at),
        header.reason.describe(),
        header.tether_version
    )?;
    writeln!(out)?;
    match &header.kind {
        Kind::Core => writeln!(
            out,
            "Everything in core changed since then is lost: accounts, characters, groups, \
             settings, the audit log, and apps installed or removed since. Apps' own data is \
             not touched. Everyone is signed out. Anything revoked since comes back: \
             permissions, group memberships, access tokens, blacklist entries and bans; \
             review them afterwards. This can't be undone."
        )?,
        Kind::Plugin(id) => {
            if tether_snapshots::set_aside_exists(db, id).await? {
                writeln!(
                    out,
                    "An earlier rollback of app {id} was cut short. Its data from before that \
                     rollback is put back first, then this snapshot is restored."
                )?;
            }
            writeln!(
                out,
                "Everything app {id} stored since then is lost. Core and other apps are not \
                 touched. This can't be undone."
            )?
        }
    }
    if !args.yes {
        write!(out, "Type yes to roll back: ")?;
        out.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        if answer.trim() != "yes" {
            writeln!(out, "Nothing was changed.")?;
            return Ok(());
        }
    }
    // Tether may have been started while the question was open.
    refuse_while_running(db).await?;
    writeln!(out, "Restoring...")?;
    snapshots
        .restore(db, snapshot, Actor::Cli)
        .await
        .with_context(|| format!("restoring {}", snapshot.name))?;
    writeln!(out, "Rolled back to {}.", snapshot.name)?;
    if header.kind == Kind::Core {
        writeln!(
            out,
            "Before starting Tether again, go back to the version that took this snapshot \
             (Tether {}; TETHER_IMAGE in deploy/.env), or it will migrate again. Then \
             `docker compose up -d`.",
            header.tether_version
        )?;
    } else {
        writeln!(
            out,
            "If the installed version of the app has migrations newer than this snapshot, they \
             run again (after a new snapshot) when Tether starts: disable the app first if you \
             don't want that. Start Tether again with `docker compose up -d`."
        )?;
    }
    Ok(())
}

/// A running server (and its plugins) would keep using data dropped and
/// recreated under it. It always holds connections: the notifications
/// listener's, and the scheduler's and workers' every few seconds.
async fn refuse_while_running(db: &PgPool) -> anyhow::Result<()> {
    let running = tether_snapshots::server_connections(db).await?;
    if running > 0 {
        bail!(
            "Tether is running ({running} connections to the database). Stop it first with \
             `docker compose stop app`, then run `docker compose run --rm app rollback`."
        );
    }
    Ok(())
}

fn pick<'a>(all: &'a [Snapshot], args: &Args) -> anyhow::Result<&'a Snapshot> {
    if let Some(name) = &args.snapshot {
        return all
            .iter()
            .find(|s| &s.name == name)
            .with_context(|| format!("no snapshot or backup is called {name}; see --list"));
    }
    let kind = match &args.plugin {
        Some(id) => Kind::Plugin(id.clone()),
        None => Kind::Core,
    };
    // Newest first.
    all.iter()
        .find(|s| s.header.kind == kind && s.header.reason == Reason::BeforeMigrations)
        .with_context(|| {
            format!(
                "there's no snapshot of {kind} taken before migrations; pick a nightly backup \
                 with --snapshot (see --list)"
            )
        })
}

fn list(all: &[Snapshot], out: &mut dyn Write) -> anyhow::Result<()> {
    if all.is_empty() {
        writeln!(out, "No snapshots or backups yet.")?;
        return Ok(());
    }
    for snapshot in all {
        writeln!(
            out,
            "{}  {}  {} UTC  {}  {} KiB",
            snapshot.name,
            snapshot.header.kind,
            snapshot.header.taken_at.format("%Y-%m-%d %H:%M"),
            snapshot.header.reason.describe(),
            snapshot.bytes.div_ceil(1024)
        )?;
    }
    Ok(())
}

fn ago(when: chrono::DateTime<Utc>) -> String {
    let minutes = (Utc::now() - when).num_minutes().max(0);
    match minutes {
        0..=1 => "just now".to_owned(),
        2..=119 => format!("{minutes} minutes ago"),
        120..=2879 => format!("{} hours ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}
