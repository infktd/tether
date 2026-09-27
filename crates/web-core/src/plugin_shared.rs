//! What apps share through the host, and nothing else: values for their
//! Secure Groups filters (per character; the host combines accounts, and this
//! never tells an app which characters share one), and timers one app publishes for
//! another to show (aa-structures feeding the timerboard).

use std::sync::Weak;

use tether_db::PgPool;
use tether_db::smart_groups::{self as db, SharedTimer};
use tether_plugins::manifest::TimersAccess;
use tether_plugins::services::{
    FilterError, FilterValue, FilterWanted, SharedTimer as WitShared, Timer, TimerError,
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
