//! What apps share through the host, and nothing else: values for their
//! Secure Groups filters (per character; the host combines accounts, and this
//! never tells an app which characters share one), and timers one app publishes for
//! another to show (aa-structures feeding the timerboard).

use std::sync::Weak;

use tether_db::PgPool;
use tether_db::smart_groups::{self as db, SharedTimer};
use tether_plugins::manifest::TimersAccess;
use tether_plugins::services::{
    Doctrine, DoctrineError, FilterError, FilterValue, FilterWanted, SharedDoctrine,
    SharedTimer as WitShared, Timer, TimerError,
};

use crate::plugins::Plugins;

/// Values one report may carry, and the largest value.
pub const MAX_VALUES: usize = 100_000;
pub const MAX_VALUE: i64 = 1_000_000_000;
/// Timers one app may publish.
pub const MAX_TIMERS: usize = 500;

pub async fn wanted(db_pool: &PgPool, plugins: &Weak<Plugins>, plugin: &str) -> Vec<FilterWanted> {
    let declares = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .is_some_and(|r| !r.manifest.filters.is_empty());
    if !declares {
        return Vec::new();
    }
    match db::app_wanted(db_pool, plugin).await {
        Ok(settings) => settings
            .into_iter()
            .map(|(name, config)| FilterWanted { name, config })
            .collect(),
        Err(err) => {
            tracing::warn!(plugin, error = %err, "reading filter settings failed");
            Vec::new()
        }
    }
}

pub async fn report(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    name: &str,
    config: &str,
    values: &[FilterValue],
) -> Result<(), FilterError> {
    let running = plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .ok_or(FilterError::Unavailable)?;
    if !running.manifest.filters.iter().any(|f| f.name == name) {
        return Err(FilterError::Invalid(format!(
            "{name} isn't a filter in plugin.toml"
        )));
    }
    if values.len() > MAX_VALUES {
        return Err(FilterError::Invalid(format!(
            "at most {MAX_VALUES} values in one report"
        )));
    }
    let wanted = db::app_wanted(db_pool, plugin)
        .await
        .map_err(|_| FilterError::Unavailable)?;
    // Only settings smart groups use: nothing else is kept.
    if !wanted.iter().any(|(n, c)| n == name && c == config) {
        return Err(FilterError::Invalid(
            "no smart group uses that setting (see filters.wanted)".to_owned(),
        ));
    }
    if values.iter().any(|v| !(0..=MAX_VALUE).contains(&v.value)) {
        return Err(FilterError::Invalid(format!("values are 0 to {MAX_VALUE}")));
    }
    let mut seen = std::collections::HashSet::new();
    if values.iter().any(|v| !seen.insert(v.character_id)) {
        return Err(FilterError::Invalid(
            "a character appears twice in one report".to_owned(),
        ));
    }
    let pairs: Vec<(i64, i64)> = values
        .iter()
        .filter(|v| v.character_id > 0)
        .map(|v| (v.character_id, v.value))
        .collect();
    let mut tx = db_pool
        .begin()
        .await
        .map_err(|_| FilterError::Unavailable)?;
    db::app_report(&mut tx, plugin, name, config, &pairs)
        .await
        .map_err(|_| FilterError::Unavailable)?;
    tx.commit().await.map_err(|_| FilterError::Unavailable)?;
    tracing::info!(
        plugin,
        filter = name,
        values = pairs.len(),
        "filter values reported"
    );
    Ok(())
}

fn access(plugins: &Weak<Plugins>, plugin: &str) -> Option<TimersAccess> {
    plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .and_then(|r| r.manifest.capabilities.timers)
}

fn text(field: &str, value: &str, max: usize) -> Result<String, TimerError> {
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(TimerError::Invalid(format!(
            "{field} is at most {max} characters, on one line"
        )));
    }
    Ok(value.to_owned())
}

pub async fn publish(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    timers: &[Timer],
) -> Result<(), TimerError> {
    if access(plugins, plugin) != Some(TimersAccess::Publish) {
        return Err(TimerError::Invalid(
            "publishing timers needs `timers = \"publish\"` in plugin.toml".to_owned(),
        ));
    }
    if timers.len() > MAX_TIMERS {
        return Err(TimerError::Invalid(format!("at most {MAX_TIMERS} timers")));
    }
    let mut rows = Vec::with_capacity(timers.len());
    for t in timers {
        if t.key.is_empty() || t.title.trim().is_empty() {
            return Err(TimerError::Invalid(
                "a timer needs a key and a title".to_owned(),
            ));
        }
        if !matches!(t.objective.as_str(), "friendly" | "hostile" | "neutral") {
            return Err(TimerError::Invalid(
                "objective is friendly, hostile or neutral".to_owned(),
            ));
        }
        let at = chrono::DateTime::parse_from_rfc3339(&t.at)
            .map_err(|_| TimerError::Invalid(format!("{:?} isn't an RFC 3339 time", t.at)))?
            .with_timezone(&chrono::Utc);
        rows.push(SharedTimer {
            plugin_id: plugin.to_owned(),
            key: text("key", &t.key, 100)?,
            title: text("title", &t.title, 200)?,
            at,
            system: text("system", &t.system, 100)?,
            details: text("details", &t.details, 1000)?,
            objective: t.objective.clone(),
            corporation_id: t.corporation_id,
        });
    }
    let mut tx = db_pool.begin().await.map_err(|_| TimerError::Unavailable)?;
    db::publish_timers(&mut tx, plugin, &rows)
        .await
        .map_err(|_| TimerError::Unavailable)?;
    tx.commit().await.map_err(|_| TimerError::Unavailable)?;
    Ok(())
}

/// Published timers a reader gets: from running publishers only, and a
/// corporation's own timers only for a viewer whose main is in it (none in
/// jobs, which have no viewer).
pub async fn published(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    viewer_corporation: Option<i64>,
) -> Result<Vec<WitShared>, TimerError> {
    if access(plugins, plugin) != Some(TimersAccess::Read) {
        return Err(TimerError::Invalid(
            "reading shared timers needs `timers = \"read\"` in plugin.toml".to_owned(),
        ));
    }
    let running = plugins.upgrade();
    let timers = db::shared_timers(db_pool)
        .await
        .map_err(|_| TimerError::Unavailable)?;
    Ok(timers
        .into_iter()
        .filter(|t| t.corporation_id.is_none() || t.corporation_id == viewer_corporation)
        .filter_map(|t| {
            let source = running.as_ref()?.running(&t.plugin_id)?;
            Some((source.manifest.plugin.name.clone(), t))
        })
        .map(|(source, t)| WitShared {
            source,
            timer: Timer {
                key: t.key,
                title: t.title,
                at: t.at.to_rfc3339(),
                system: t.system,
                details: t.details,
                objective: t.objective,
                corporation_id: t.corporation_id,
            },
        })
        .collect())
}

// ---- doctrines ------------------------------------------------------------------

/// Doctrines one app may publish.
pub const MAX_DOCTRINES: usize = 500;
/// Groups one doctrine may be limited to.
const MAX_DOCTRINE_GROUPS: usize = 100;

fn doctrine_access(plugins: &Weak<Plugins>, plugin: &str) -> Option<TimersAccess> {
    plugins
        .upgrade()
        .and_then(|p| p.running(plugin))
        .and_then(|r| r.manifest.capabilities.doctrines)
}

fn doctrine_text(field: &str, value: &str, max: usize) -> Result<String, DoctrineError> {
    let value = value.trim();
    // Invisible and direction-changing characters too: Fleet Pings refuses
    // them, and they'd make lookalike names.
    if value.is_empty() || value.chars().count() > max || value.chars().any(crate::pings::invisible)
    {
        return Err(DoctrineError::Invalid(format!(
            "a doctrine's {field} is 1 to {max} characters, on one line"
        )));
    }
    Ok(value.to_owned())
}

/// Replaces `plugin`'s shared doctrines (allianceauth-fittings' doctrines,
/// for aa-fleetpings and aa-fat). `see_all` must be one of its own
/// permissions.
pub async fn publish_doctrines(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    doctrines: &[Doctrine],
    see_all: Option<&str>,
) -> Result<(), DoctrineError> {
    if doctrine_access(plugins, plugin) != Some(TimersAccess::Publish) {
        return Err(DoctrineError::Invalid(
            "publishing doctrines needs `doctrines = \"publish\"` in plugin.toml".to_owned(),
        ));
    }
    if doctrines.len() > MAX_DOCTRINES {
        return Err(DoctrineError::Invalid(format!(
            "at most {MAX_DOCTRINES} doctrines"
        )));
    }
    if let Some(permission) = see_all {
        let declared = plugins
            .upgrade()
            .and_then(|p| p.running(plugin))
            .is_some_and(|r| r.manifest.permissions.contains_key(permission));
        if !declared {
            return Err(DoctrineError::Invalid(format!(
                "{permission:?} isn't one of this app's permissions"
            )));
        }
    }
    // A name another app publishes stays that app's: its link can't be
    // taken over.
    let taken = tether_db::doctrines::names_of_others(db_pool, plugin)
        .await
        .map_err(|e| {
            tracing::error!(plugin, error = %e, "reading shared doctrine names");
            DoctrineError::Unavailable
        })?;
    let mut rows: Vec<tether_db::doctrines::NewDoctrine> = Vec::with_capacity(doctrines.len());
    for d in doctrines {
        let key = doctrine_text("key", &d.key, 100)?;
        if rows.iter().any(|r| r.key == key) {
            return Err(DoctrineError::Invalid(format!(
                "the key {key:?} is used twice"
            )));
        }
        tether_plugins::page::check_link_path(&d.link)
            .map_err(|p| DoctrineError::Invalid(p.to_string()))?;
        if let Some(groups) = &d.groups
            && (groups.len() > MAX_DOCTRINE_GROUPS || groups.iter().any(|g| *g <= 0))
        {
            return Err(DoctrineError::Invalid(format!(
                "a doctrine is limited to at most {MAX_DOCTRINE_GROUPS} groups, by id"
            )));
        }
        let name = doctrine_text("name", &d.name, 100)?;
        if let Some((other, _)) = taken
            .iter()
            .find(|(_, taken)| *taken == name.to_lowercase())
        {
            return Err(DoctrineError::Invalid(format!(
                "{name:?} is already published by {other}"
            )));
        }
        rows.push(tether_db::doctrines::NewDoctrine {
            key,
            name,
            link: d.link.clone(),
            groups: d.groups.clone(),
        });
    }
    tether_db::doctrines::replace(db_pool, plugin, &rows, see_all)
        .await
        .map_err(|e| {
            tracing::error!(plugin, error = %e, "publishing doctrines");
            DoctrineError::Unavailable
        })
}

/// A shared doctrine an account may see, from a running app: its name, the
/// publishing app, and its page's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenDoctrine {
    pub name: String,
    pub source: String,
    /// `/plugins/<id>/<link>`.
    pub path: String,
}

/// The shared doctrines `account` may see (for Fleet Pings, and apps that
/// read them): everyone's, those limited to one of its groups, and every
/// one of a publisher whose see-all permission it holds.
pub async fn doctrines_seen(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    account: tether_db::accounts::AccountId,
) -> Result<Vec<SeenDoctrine>, sqlx::Error> {
    // Groups count only while the account has a main, as everywhere else.
    let has_main = tether_db::accounts::get(db_pool, account)
        .await?
        .is_some_and(|a| a.main.is_some());
    let groups: Vec<i64> = if has_main {
        tether_db::groups::of_account(db_pool, account)
            .await?
            .into_iter()
            .map(|(id, _)| id.0)
            .collect()
    } else {
        Vec::new()
    };
    let held: Vec<String> = tether_db::permissions::effective(db_pool, account)
        .await?
        .into_iter()
        .collect();
    let running = plugins.upgrade();
    let mut seen: Vec<SeenDoctrine> = Vec::new();
    for d in tether_db::doctrines::seen(db_pool, &groups, &held).await? {
        // From apps running and still publishing.
        let publishing = running
            .as_ref()
            .and_then(|p| p.running(&d.plugin_id))
            .is_some_and(|r| r.manifest.capabilities.doctrines == Some(TimersAccess::Publish));
        // One of a name (an app may name two alike): the first.
        if !publishing || seen.iter().any(|s| s.name.eq_ignore_ascii_case(&d.name)) {
            continue;
        }
        seen.push(SeenDoctrine {
            path: crate::plugins::page_href(&d.plugin_id, &d.link),
            name: d.name,
            source: d.plugin_name,
        });
    }
    Ok(seen)
}

/// Shared doctrines for `plugin` to offer: those its viewer may see; none
/// in a job.
pub async fn published_doctrines(
    db_pool: &PgPool,
    plugins: &Weak<Plugins>,
    plugin: &str,
    account: Option<i64>,
) -> Result<Vec<SharedDoctrine>, DoctrineError> {
    if doctrine_access(plugins, plugin) != Some(TimersAccess::Read) {
        return Err(DoctrineError::Invalid(
            "reading shared doctrines needs `doctrines = \"read\"` in plugin.toml".to_owned(),
        ));
    }
    let Some(account) = account else {
        return Ok(Vec::new());
    };
    let seen = doctrines_seen(db_pool, plugins, tether_db::accounts::AccountId(account))
        .await
        .map_err(|e| {
            tracing::error!(plugin, error = %e, "reading shared doctrines");
            DoctrineError::Unavailable
        })?;
    Ok(seen
        .into_iter()
        .map(|d| SharedDoctrine {
            name: d.name,
            link: d.path,
            source: d.source,
        })
        .collect())
}
