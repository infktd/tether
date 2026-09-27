//! Item types from ESI's public universe endpoints (no token), kept in the
//! app's storage: types don't change between patches, so each name and
//! type is asked of ESI once.
//!
//! - Adding a fit turns its item names into type ids with one
//!   `universe-ids` call (names seen before are taken from storage).
//! - The `details` job then reads each new type's group and required
//!   skills (`universe-type`, and `universe-group` for a group's category),
//!   and the skills' names (`esi::names`), within the host's 100 ESI calls
//!   a run, queuing itself again while more are left.

use std::collections::{BTreeMap, BTreeSet};

use tether_plugin_sdk::esi::{self, Error as EsiError, Subject};
use tether_plugin_sdk::jobs::{self, JobError, NewJob};
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::{self, Value as Db};

use crate::{id_list, int, text};

/// The job (and the daily schedule) that looks up new types.
pub(crate) const DETAILS: &str = "details";
/// ESI calls one `details` run makes, at most (the host allows 100).
const BUDGET: usize = 90;
/// Types one run looks at, at most.
const BATCH: i64 = 200;
/// Names one `universe-ids` call takes (ESI's limit).
pub(crate) const MAX_NAMES: usize = 500;

/// dogma attributes: requiredSkill1..3 and their levels.
const SKILL_ATTRIBUTES: [(i64, i64); 3] = [(182, 277), (183, 278), (184, 279)];

/// Public endpoints don't read the subject.
const PUBLIC: Subject = Subject::Character(0);

#[derive(Debug)]
pub(crate) enum Failure {
    /// More different names than one lookup takes.
    TooMany,
    /// ESI or storage didn't answer.
    Unavailable(String),
}

/// The items among `names`: each name, lowercased, to its type id and
/// its name as EVE writes it. Names that aren't items are left out.
pub(crate) fn resolve(names: &[String]) -> Result<BTreeMap<String, (i64, String)>, Failure> {
    let mut wanted: BTreeMap<String, &str> = BTreeMap::new();
    for name in names {
        wanted.entry(name.to_lowercase()).or_insert(name);
    }
    let keys: Vec<&str> = wanted.keys().map(String::as_str).collect();
    let stored = storage::query(
        "SELECT type_id, name, lower(name) FROM types \
         WHERE lower(name) = ANY(string_to_array($1, chr(10)))",
        &[keys.join("\n").into()],
    )
    .map_err(|e| Failure::Unavailable(format!("reading types: {e:?}")))?;
    let mut found: BTreeMap<String, (i64, String)> = stored
        .rows
        .iter()
        .map(|r| (text(r, 2), (int(r, 0), text(r, 1))))
        .collect();
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|(lower, _)| !found.contains_key(*lower))
        .map(|(_, name)| *name)
        .collect();
    if missing.is_empty() {
        return Ok(found);
    }
    if missing.len() > MAX_NAMES {
        return Err(Failure::TooMany);
    }
    let answer = esi::get(
        "universe-ids",
        PUBLIC,
        &[("names".to_owned(), missing.join("\n"))],
        None,
    )
    .map_err(|e| Failure::Unavailable(format!("universe-ids: {e:?}")))?;
    let body: serde_json::Value = serde_json::from_str(&answer.body)
        .map_err(|e| Failure::Unavailable(format!("universe-ids: {e}")))?;
    let mut new = Vec::new();
    for item in body["inventory_types"].as_array().into_iter().flatten() {
        let (Some(id), Some(name)) = (item["id"].as_i64(), item["name"].as_str()) else {
            continue;
        };
        let lower = name.to_lowercase();
        if id > 0 && wanted.contains_key(&lower) {
            found.insert(lower, (id, name.to_owned()));
            new.push(serde_json::json!({ "type_id": id, "name": name }));
        }
    }
    if !new.is_empty() {
        storage::execute(
            "INSERT INTO types (type_id, name) \
             SELECT type_id, name FROM json_to_recordset($1::json) AS x(type_id bigint, name text) \
             ON CONFLICT (type_id) DO UPDATE SET name = EXCLUDED.name",
            &[Db::json(serde_json::Value::Array(new).to_string())],
        )
        .map_err(|e| Failure::Unavailable(format!("storing types: {e:?}")))?;
    }
    Ok(found)
}

/// Queues `details` to run now (after a fit is saved).
pub(crate) fn queue_details() {
    if let Err(err) = jobs::enqueue(NewJob::new(DETAILS).key(DETAILS)) {
        log::warn(format!("queuing the item lookup: {err:?}"));
    }
}

/// A type's required skills, from its dogma attributes: `[[skill, level]]`.
pub(crate) fn required_skills(item: &serde_json::Value) -> Vec<(i64, i64)> {
    let attribute = |id: i64| -> Option<f64> {
        item["dogma_attributes"]
            .as_array()?
            .iter()
            .find(|a| a["attribute_id"].as_i64() == Some(id))?["value"]
            .as_f64()
    };
    SKILL_ATTRIBUTES
        .iter()
        .filter_map(|(skill, level)| {
            // Whole numbers stored as doubles.
            let skill = attribute(*skill)?.round() as i64;
            let level = attribute(*level).unwrap_or(1.0).round() as i64;
            (skill > 0).then_some((skill, level.clamp(1, 5)))
        })
        .collect()
}

fn retry(what: &str, err: impl std::fmt::Debug) -> JobError {
    JobError::Retry(format!("{what}: {err:?}"))
}

/// Looks up the types fits use that haven't been, and the names of the
/// skills they need.
pub(crate) fn details() -> Result<(), JobError> {
    let pending: Vec<i64> = storage::query(
        "SELECT type_id FROM types WHERE looked_up_at IS NULL AND type_id IN \
         (SELECT hull_type_id FROM fits UNION SELECT type_id FROM fit_items \
          UNION SELECT charge_type_id FROM fit_items WHERE charge_type_id IS NOT NULL) \
         ORDER BY type_id LIMIT $1",
        &[BATCH.into()],
    )
    .map_err(|e| retry("reading types", e))?
    .rows
    .iter()
    .map(|r| int(r, 0))
    .collect();
    let mut groups: BTreeSet<i64> = storage::query("SELECT group_id FROM item_groups", &[])
        .map_err(|e| retry("reading groups", e))?
        .rows
        .iter()
        .map(|r| int(r, 0))
        .collect();
    let mut calls = 0;
    let mut more = pending.len() as i64 == BATCH;
    for id in pending {
        // A type and, perhaps, its group.
        if calls + 2 > BUDGET {
            more = true;
            break;
        }
        calls += 1;
        let item = match esi::get(
            "universe-type",
            PUBLIC,
            &[("type_id".to_owned(), id.to_string())],
            None,
        ) {
            Ok(answer) => serde_json::from_str::<serde_json::Value>(&answer.body)
                .map_err(|e| retry("reading a type", e))?,
            // ESI doesn't know it (any more): nothing to show but its name.
            Err(EsiError::Status(404)) => {
                log::warn(format!("ESI doesn't know type {id}"));
                storage::execute(
                    "UPDATE types SET skills = '[]', looked_up_at = now() WHERE type_id = $1",
                    &[id.into()],
                )
                .map_err(|e| retry("storing a type", e))?;
                continue;
            }
            // What's done is kept; the rest waits for the retry.
            Err(err) => return Err(retry("looking up a type", err)),
        };
        let group = item["group_id"].as_i64().filter(|g| *g > 0);
        if let Some(group) = group
            && !groups.contains(&group)
        {
            calls += 1;
            match esi::get(
                "universe-group",
                PUBLIC,
                &[("group_id".to_owned(), group.to_string())],
                None,
            ) {
                Ok(answer) => {
                    let body: serde_json::Value = serde_json::from_str(&answer.body)
                        .map_err(|e| retry("reading a group", e))?;
                    storage::execute(
                        "INSERT INTO item_groups (group_id, category_id, name) VALUES ($1, $2, $3) \
                         ON CONFLICT (group_id) DO UPDATE SET category_id = EXCLUDED.category_id, \
                         name = EXCLUDED.name",
                        &[
                            group.into(),
                            body["category_id"].as_i64().unwrap_or_default().into(),
                            body["name"].as_str().unwrap_or_default().into(),
                        ],
                    )
                    .map_err(|e| retry("storing a group", e))?;
                    groups.insert(group);
                }
                Err(err) => return Err(retry("looking up a group", err)),
            }
        }
        let skills: Vec<serde_json::Value> = required_skills(&item)
            .into_iter()
            .map(|(skill, level)| serde_json::json!([skill, level]))
            .collect();
        let mut params: Vec<Db> = vec![
            id.into(),
            group.into(),
            Db::json(serde_json::Value::Array(skills).to_string()),
        ];
        let name = item["name"].as_str().filter(|n| !n.is_empty());
        params.push(name.map(str::to_owned).into());
        storage::execute(
            "UPDATE types SET group_id = $2, skills = $3::jsonb, name = coalesce($4, name), \
             looked_up_at = now() WHERE type_id = $1",
            &params,
        )
        .map_err(|e| retry("storing a type", e))?;
    }
    // The names of the skills fits need.
    let skills: Vec<i64> = storage::query(
        "SELECT DISTINCT (s->>0)::bigint FROM types t, jsonb_array_elements(t.skills) s \
         WHERE t.skills IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM types k WHERE k.type_id = (s->>0)::bigint) \
         LIMIT 1000",
        &[],
    )
    .map_err(|e| retry("reading skills", e))?
    .rows
    .iter()
    .map(|r| int(r, 0))
    .filter(|id| *id > 0)
    .collect();
    if !skills.is_empty() {
        if calls < BUDGET {
            let named = esi::names(&skills).map_err(|e| retry("naming skills", e))?;
            let rows: Vec<serde_json::Value> = named
                .into_iter()
                .map(|n| serde_json::json!({ "type_id": n.id, "name": n.name }))
                .collect();
            storage::execute(
                "INSERT INTO types (type_id, name) \
                 SELECT type_id, name FROM json_to_recordset($1::json) AS x(type_id bigint, name text) \
                 ON CONFLICT (type_id) DO NOTHING",
                &[Db::json(serde_json::Value::Array(rows).to_string())],
            )
            .map_err(|e| retry("storing skill names", e))?;
        } else {
            more = true;
        }
    }
    if more {
        jobs::enqueue(NewJob::new(DETAILS).key("details_more"))
            .map_err(|e| retry("queuing the next run", e))?;
    }
    log::info(format!("looked up item details with {calls} ESI calls"));
    Ok(())
}

/// Whether any of `ids` (types) hasn't been looked up yet, or needs a
/// skill whose name isn't known yet.
pub(crate) fn pending(ids: &[i64]) -> bool {
    storage::query(
        "SELECT 1 FROM types t WHERE t.type_id = ANY(string_to_array($1, ',')::bigint[]) \
         AND (t.looked_up_at IS NULL OR EXISTS (SELECT 1 FROM jsonb_array_elements(t.skills) s \
              WHERE NOT EXISTS (SELECT 1 FROM types k WHERE k.type_id = (s->>0)::bigint))) \
         LIMIT 1",
        &[id_list(ids).into()],
    )
    .map(|r| !r.rows.is_empty())
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn required_skills_come_from_dogma_attributes() {
        let item = serde_json::json!({
            "dogma_attributes": [
                {"attribute_id": 182, "value": 3318.0},
                {"attribute_id": 277, "value": 5.0},
                {"attribute_id": 183, "value": 3300.0},
                {"attribute_id": 278, "value": 2.0},
                {"attribute_id": 9, "value": 350.0}
            ]
        });
        assert_eq!(required_skills(&item), vec![(3318, 5), (3300, 2)]);
        assert!(required_skills(&serde_json::json!({})).is_empty());
        // A skill without its level needs level 1.
        let item =
            serde_json::json!({"dogma_attributes": [{"attribute_id": 184, "value": 3301.0}]});
        assert_eq!(required_skills(&item), vec![(3301, 1)]);
    }
}
