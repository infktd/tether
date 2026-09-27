//! Secure Groups filters (aa-securegroups' Member Audit filters): a skill at
//! a level, a skill set, an item in the assets. An hourly job reports each
//! synced character's answer.

use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::{self, Value as Db};

use crate::{id_list, int, retry};

/// Filter settings reported per run: the host takes at most 50 reports in
/// one call; more wait for a follow-up run.
const REPORTS_PER_RUN: usize = 50;
/// The follow-up run's job, and its key (queuing again replaces it).
pub(crate) const MORE_REPORTS: &str = "report_filters_more";
/// Rows read at once (the host returns at most 5,000).
const PAGE_ROWS: usize = 5000;

/// Hourly: each filter setting smart groups use, answered for every
/// synced character (1 or 0; a reversed filter needs every character
/// reported). At most [`REPORTS_PER_RUN`] settings a run, from `from` in
/// a stable order; the rest go to a follow-up run.
pub(crate) fn report_filters(from: usize) -> Result<(), JobError> {
    let mut wanted = tether_plugin_sdk::filters::wanted();
    wanted.sort_by(|a, b| (&a.name, &a.config).cmp(&(&b.name, &b.config)));
    for setting in wanted.iter().skip(from).take(REPORTS_PER_RUN) {
        let Some(values) = filter_values(&setting.name, &setting.config)? else {
            continue;
        };
        match tether_plugin_sdk::filters::report(&setting.name, &setting.config, &values) {
            Ok(()) => {}
            // No group uses it any more (changed since `wanted`): skip it.
            Err(tether_plugin_sdk::filters::Error::Invalid(why)) => {
                log::warn(format!("a {} filter wasn't reported: {why}", setting.name));
            }
            Err(err) => return Err(retry("reporting a filter", err)),
        }
    }
    let next = from.saturating_add(REPORTS_PER_RUN);
    if wanted.len() > next {
        jobs::enqueue(
            NewJob::new(MORE_REPORTS)
                .key(MORE_REPORTS)
                .payload(serde_json::json!({ "from": next }).to_string()),
        )
        .map_err(|e| retry("queuing more filter reports", e))?;
    }
    Ok(())
}

/// One setting's values, or `None` for a setting that isn't reported now
/// (a level out of range, say, or names still being learned): the host
/// then leaves groups using it alone rather than judge on missing data.
fn filter_values(name: &str, config: &str) -> Result<Option<Vec<(i64, i64)>>, JobError> {
    let config: serde_json::Value = serde_json::from_str(config).unwrap_or_default();
    let field = |key: &str| {
        config
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    // Whether the character `c` passes, with its parameters after `$1`,
    // and which characters have the data to say (a first sync that failed,
    // or assets read only in part, mustn't read as "has none": a reversed
    // filter would then let the account in).
    let (condition, params, whole): (&str, Vec<Db>, &str) = match name {
        "skill" => {
            let level = config
                .get("level")
                .and_then(serde_json::Value::as_i64)
                .filter(|l| (1..=5).contains(l));
            let (Some(skill), Some(level)) = (field("skill"), level) else {
                log::warn(format!(
                    "ignoring a skill filter setting (it needs a skill and a level from 1 to 5): {config}"
                ));
                return Ok(None);
            };
            let Some(ids) = type_ids(skill, Stored::Skills)? else {
                return Ok(None);
            };
            (
                "EXISTS (SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
                   AND s.skill_id = ANY(string_to_array($2, ',')::bigint[]) AND s.trained_level >= $3)",
                vec![id_list(&ids).into(), level.into()],
                "c.skills_at IS NOT NULL",
            )
        }
        "skill_set" => {
            let Some(set) = field("skill_set") else {
                log::warn(format!("ignoring a skill set filter setting: {config}"));
                return Ok(None);
            };
            let found = storage::query("SELECT id FROM skill_sets WHERE name = $1", &[set.into()])
                .map_err(|e| retry("reading skill sets", e))?;
            // A set that's gone (deleted, renamed, mistyped) isn't
            // answered: groups using it wait rather than let everyone
            // through a reversed filter.
            let Some(id) = found.rows.first().map(|r| int(r, 0)) else {
                log::warn(format!(
                    "skill set filter: there's no skill set named {set:?}, so that filter isn't \
                     answered (its groups wait)"
                ));
                return Ok(None);
            };
            // As the Skill Sets page: every skill of the set at its level.
            (
                "EXISTS (SELECT 1 FROM skill_sets ss WHERE ss.id = $2) AND NOT EXISTS ( \
                   SELECT 1 FROM skill_set_skills k WHERE k.set_id = $2 AND NOT EXISTS ( \
                     SELECT 1 FROM skills s WHERE s.character_id = c.character_id \
                       AND s.skill_id = k.skill_id AND s.active_level >= k.level))",
                vec![id.into()],
                "c.skills_at IS NOT NULL",
            )
        }
        "asset" => {
            let Some(item) = field("item") else {
                log::warn(format!("ignoring an asset filter setting: {config}"));
                return Ok(None);
            };
            let Some(ids) = type_ids(item, Stored::Assets)? else {
                return Ok(None);
            };
            (
                "EXISTS (SELECT 1 FROM assets a WHERE a.character_id = c.character_id \
                   AND a.type_id = ANY(string_to_array($2, ',')::bigint[]))",
                vec![id_list(&ids).into()],
                "c.assets_at IS NOT NULL",
            )
        }
        other => {
            log::warn(format!("no filter {other}"));
            return Ok(None);
        }
    };
    // Every character with the data, page by page.
    let sql = format!(
        "SELECT c.character_id, CASE WHEN {condition} THEN 1 ELSE 0 END FROM characters c \
         WHERE {whole} AND c.character_id > $1 \
         ORDER BY c.character_id LIMIT {PAGE_ROWS}"
    );
    let mut values = Vec::new();
    let mut after = 0i64;
    loop {
        let mut p: Vec<Db> = vec![after.into()];
        p.extend(params.iter().cloned());
        let rows = storage::query(&sql, &p).map_err(|e| retry("answering a filter", e))?;
        values.extend(rows.rows.iter().map(|r| (int(r, 0), int(r, 1))));
        match rows.rows.last() {
            Some(last) if rows.rows.len() >= PAGE_ROWS => after = int(last, 0),
            _ => break,
        }
    }
    Ok(Some(values))
}

/// Where a filter's type ids are stored.
#[derive(Clone, Copy)]
enum Stored {
    Skills,
    Assets,
}

/// The type ids named `name` (whole, case aside). An unknown name is
/// nobody's (every character 0), unless some stored ids have no name
/// yet: it may be one of them, so `None` (not reported) until they do.
fn type_ids(name: &str, stored: Stored) -> Result<Option<Vec<i64>>, JobError> {
    let rows = storage::query(
        "SELECT id FROM names WHERE lower(name) = lower($1) AND category = 'inventory_type' LIMIT 20",
        &[name.into()],
    )
    .map_err(|e| retry("reading names", e))?;
    let ids: Vec<i64> = rows.rows.iter().map(|r| int(r, 0)).collect();
    if !ids.is_empty() {
        return Ok(Some(ids));
    }
    // Fixed SQL per kind, never data.
    let (what, unnamed) = match stored {
        Stored::Skills => (
            "a skill",
            "SELECT 1 FROM skills s WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = s.skill_id) LIMIT 1",
        ),
        Stored::Assets => (
            "an item",
            "SELECT 1 FROM assets a WHERE NOT EXISTS (SELECT 1 FROM names n WHERE n.id = a.type_id) LIMIT 1",
        ),
    };
    let pending = storage::query(unnamed, &[]).map_err(|e| retry("reading names", e))?;
    if pending.rows.is_empty() {
        log::warn(format!(
            "no member character has {what} named {name:?}, so nobody passes that filter"
        ));
        Ok(Some(Vec::new()))
    } else {
        log::warn(format!(
            "no member character has {what} named {name:?} yet, but some names are still being \
             learned: that filter isn't answered until they are"
        ));
        Ok(None)
    }
}
