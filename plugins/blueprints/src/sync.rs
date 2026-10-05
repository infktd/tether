//! Reading the owners' blueprints, running jobs and where the blueprints
//! are, as aa-blueprints' tasks do: blueprints every 3 hours, jobs every
//! hour, places every 12. Corporate owners are the app's data sources
//! (one per corporation); personal owners are characters registered for
//! the app that their pilot added.

use std::collections::HashMap;

use tether_plugin_sdk::esi::{self, Character, Subject};
use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::{self, Value as Db};

use crate::{int, retry, text};

const PUBLIC: Subject = Subject::Character(0);
/// Places named per run, within a run's 100 ESI calls.
const PLACES_PER_RUN: i64 = 40;
/// Item ids `corporation-asset-places` takes at once.
const PLACE_IDS_PER_CALL: usize = 1000;

/// An owner whose blueprints are read, and the subject to read them as.
pub struct Owner {
    pub kind: &'static str,
    pub id: i64,
    pub name: String,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub subject: Subject,
}

/// Corporate owners (each corporation once, through its first data
/// source) and personal owners still registered for the app. The second
/// list is personal owners no longer registered (their pilot left, or
/// their token lost a scope): kept, but not read.
pub fn owners() -> Result<(Vec<Owner>, Vec<i64>), JobError> {
    let mut out: Vec<Owner> = Vec::new();
    for source in esi::data_sources() {
        if !out.iter().any(|o| o.id == source.corporation_id) {
            out.push(Owner {
                kind: "corporation",
                id: source.corporation_id,
                name: String::new(),
                corporation_id: source.corporation_id,
                alliance_id: source.alliance_id,
                subject: Subject::DataSource(source.id),
            });
        }
    }
    let registered: Vec<Character> = esi::characters();
    let added = storage::query("SELECT character_id FROM personal_owners", &[])
        .map_err(|e| retry("reading personal owners", e))?;
    let mut lapsed = Vec::new();
    for row in &added.rows {
        let id = int(row, 0);
        match registered.iter().find(|c| c.id == id) {
            Some(c) => out.push(Owner {
                kind: "character",
                id,
                name: c.name.clone(),
                corporation_id: c.corporation_id,
                alliance_id: c.alliance_id,
                subject: Subject::Character(id),
            }),
            None => lapsed.push(id),
        }
    }
    // The least recently read first, so a run that runs out of its ESI
    // budget doesn't starve the same owners every time.
    let read = storage::query("SELECT kind, id, read_at FROM owners", &[])
        .map_err(|e| retry("reading owners", e))?;
    let read_at = |o: &Owner| {
        read.rows
            .iter()
            .find(|r| text(r, 0) == o.kind && int(r, 1) == o.id)
            .map(|r| text(r, 2))
            .unwrap_or_default()
    };
    out.sort_by_cached_key(read_at);
    Ok((out, lapsed))
}

/// Owners no longer in use (a data source gone, a character no longer
/// registered) are hidden at once and forgotten a week later, with their
/// blueprints and requests (as in AA), so a blip loses nothing. A
/// personal owner a pilot removed goes at once (on Owners).
fn forget_gone(owners: &[Owner], lapsed: &[i64]) -> Result<(), JobError> {
    let corporations: Vec<i64> = owners
        .iter()
        .filter(|o| o.kind == "corporation")
        .map(|o| o.id)
        .collect();
    let characters: Vec<i64> = owners
        .iter()
        .filter(|o| o.kind == "character")
        .map(|o| o.id)
        .collect();
    storage::transaction(&[
        storage::Statement::new(
            "UPDATE owners SET missing_since = CASE \
                 WHEN (kind = 'corporation' AND id = ANY(SELECT jsonb_array_elements_text($1::jsonb)::bigint)) \
                   OR (kind = 'character' AND id = ANY(SELECT jsonb_array_elements_text($2::jsonb)::bigint)) \
                 THEN NULL ELSE coalesce(missing_since, now()) END",
            vec![
                Db::json(serde_json::json!(corporations).to_string()),
                Db::json(serde_json::json!(characters).to_string()),
            ],
        ),
        // A lapsed personal owner past the week is forgotten as an owner
        // too: registered again (perhaps on another account), it's added
        // again by its pilot.
        storage::Statement::new(
            "DELETE FROM personal_owners WHERE character_id IN ( \
                 SELECT id FROM owners WHERE kind = 'character' \
                   AND missing_since < now() - interval '7 days') \
               AND character_id = ANY(SELECT jsonb_array_elements_text($1::jsonb)::bigint)",
            vec![Db::json(serde_json::json!(lapsed).to_string())],
        ),
        storage::Statement::new(
            "DELETE FROM owners WHERE missing_since < now() - interval '7 days' \
                OR (kind = 'character' AND id NOT IN (SELECT character_id FROM personal_owners))",
            vec![],
        ),
    ])
    .map_err(|e| retry("forgetting owners", e))?;
    Ok(())
}

fn note_owner(owner: &Owner, error: Option<String>) -> Result<(), JobError> {
    storage::execute(
        "INSERT INTO owners (kind, id, name, corporation_id, alliance_id, read_at, error) \
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $6::text IS NULL THEN now() END, $6) \
         ON CONFLICT (kind, id) DO UPDATE SET \
             name = CASE WHEN EXCLUDED.name = '' THEN owners.name ELSE EXCLUDED.name END, \
             corporation_id = EXCLUDED.corporation_id, alliance_id = EXCLUDED.alliance_id, \
             read_at = coalesce(EXCLUDED.read_at, owners.read_at), error = EXCLUDED.error",
        &[
            owner.kind.into(),
            owner.id.into(),
            owner.name.clone().into(),
            owner.corporation_id.into(),
            owner.alliance_id.into(),
            error.into(),
        ],
    )
    .map_err(|e| retry("noting an owner", e))?;
    Ok(())
}

fn pages(bodies: Vec<String>) -> Vec<serde_json::Value> {
    bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .filter_map(|v| v.as_array().cloned())
        .flatten()
        .collect()
}

fn set_error(column: &str, problems: &[String]) -> Result<(), JobError> {
    let error = (!problems.is_empty()).then(|| problems.join("; "));
    if let Some(error) = &error {
        log::warn(error);
    }
    storage::execute(
        &format!("UPDATE settings SET {column} = now(), sync_error = $1 WHERE id = 1"),
        &[error.into()],
    )
    .map_err(|e| retry("noting the sync", e))?;
    Ok(())
}

// ---- blueprints ------------------------------------------------------------

pub fn blueprints() -> Result<(), JobError> {
    let (owners, lapsed) = owners()?;
    forget_gone(&owners, &lapsed)?;
    let mut problems = Vec::new();
    for owner in &owners {
        let endpoint = if owner.kind == "corporation" {
            "corporation-blueprints"
        } else {
            "character-blueprints"
        };
        match esi::get_all(endpoint, owner.subject, &[]) {
            Ok(bodies) => {
                note_owner(owner, None)?;
                store_blueprints(owner, &pages(bodies))?;
            }
            Err(err) => {
                let why = format!("{err:?}");
                problems.push(format!(
                    "{} {}: blueprints not read: {why}",
                    owner.kind, owner.id
                ));
                note_owner(owner, Some(why))?;
            }
        }
    }
    learn_names()?;
    learn_products()?;
    name_places(&owners)?;
    set_error("blueprints_at", &problems)
}

/// Replaces the owner's blueprints with what ESI said: those gone go
/// (with their requests, as in AA), the rest are updated. A blueprint
/// that moved to another owner moves with it.
fn store_blueprints(owner: &Owner, list: &[serde_json::Value]) -> Result<(), JobError> {
    let rows: Vec<serde_json::Value> = list
        .iter()
        .filter_map(|b| {
            let runs = b["runs"].as_i64().filter(|r| *r > 0);
            let quantity = b["quantity"].as_i64().filter(|q| *q > 0).unwrap_or(1);
            Some(serde_json::json!({
                "item_id": b["item_id"].as_i64()?,
                "type_id": b["type_id"].as_i64()?,
                "location_id": b["location_id"].as_i64()?,
                "location_flag": b["location_flag"].as_str().unwrap_or("Undefined"),
                "quantity": quantity,
                "runs": runs,
                "me": b["material_efficiency"].as_i64().unwrap_or(0),
                "te": b["time_efficiency"].as_i64().unwrap_or(0),
            }))
        })
        .collect();
    let ids: Vec<i64> = rows.iter().filter_map(|r| r["item_id"].as_i64()).collect();
    storage::transaction(&[
        storage::Statement::new(
            "DELETE FROM blueprints WHERE owner_kind = $1 AND owner_id = $2 \
             AND NOT (item_id = ANY(SELECT jsonb_array_elements_text($3::jsonb)::bigint))",
            vec![
                owner.kind.into(),
                owner.id.into(),
                Db::json(serde_json::json!(ids).to_string()),
            ],
        ),
        storage::Statement::new(
            "INSERT INTO blueprints (item_id, owner_kind, owner_id, type_id, location_id, \
                 location_flag, quantity, runs, material_efficiency, time_efficiency) \
             SELECT DISTINCT ON (item_id) item_id, $2, $3, type_id, location_id, location_flag, \
                 quantity, runs, me, te \
             FROM json_to_recordset($1::json) AS x(item_id bigint, type_id bigint, \
                 location_id bigint, location_flag text, quantity integer, runs integer, \
                 me integer, te integer) \
             ON CONFLICT (item_id) DO UPDATE SET owner_kind = EXCLUDED.owner_kind, \
                 owner_id = EXCLUDED.owner_id, type_id = EXCLUDED.type_id, \
                 place_id = CASE WHEN blueprints.location_id = EXCLUDED.location_id \
                     THEN blueprints.place_id END, \
                 within = CASE WHEN blueprints.location_id = EXCLUDED.location_id \
                     THEN blueprints.within END, \
                 location_id = EXCLUDED.location_id, location_flag = EXCLUDED.location_flag, \
                 quantity = EXCLUDED.quantity, runs = EXCLUDED.runs, \
                 material_efficiency = EXCLUDED.material_efficiency, \
                 time_efficiency = EXCLUDED.time_efficiency",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                owner.kind.into(),
                owner.id.into(),
            ],
        ),
    ])
    .map_err(|e| retry("storing blueprints", e))?;
    Ok(())
}

// ---- jobs ------------------------------------------------------------------

/// Jobs still on a blueprint: running, paused, or done but not delivered.
fn running(status: &str) -> bool {
    matches!(status, "active" | "paused" | "ready")
}

pub fn jobs() -> Result<(), JobError> {
    let (owners, _lapsed) = owners()?;
    let mut problems = Vec::new();
    for owner in &owners {
        let read = if owner.kind == "corporation" {
            esi::get_all("corporation-industry-jobs", owner.subject, &[]).map(pages)
        } else {
            esi::get("character-industry-jobs", owner.subject, &[], None).map(|r| {
                serde_json::from_str::<serde_json::Value>(&r.body)
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
            })
        };
        match read {
            Ok(list) => store_jobs(owner, &list)?,
            Err(err) => problems.push(format!(
                "{} {}: jobs not read: {err:?}",
                owner.kind, owner.id
            )),
        }
    }
    learn_names()?;
    // Places that couldn't be named are tried again every hour.
    name_places(&owners)?;
    set_error("jobs_at", &problems)
}

/// Replaces the jobs on the owner's blueprints: only jobs on its own
/// blueprints count (AA rejects personal listings of corporate jobs and
/// the other way round).
fn store_jobs(owner: &Owner, list: &[serde_json::Value]) -> Result<(), JobError> {
    let rows: Vec<serde_json::Value> = list
        .iter()
        .filter(|j| running(j["status"].as_str().unwrap_or_default()))
        .filter_map(|j| {
            Some(serde_json::json!({
                "job_id": j["job_id"].as_i64()?,
                "item_id": j["blueprint_id"].as_i64()?,
                "activity": j["activity_id"].as_i64()?,
                "installer_id": j["installer_id"].as_i64()?,
                "runs": j["runs"].as_i64()?,
                "start_date": j["start_date"].as_str()?,
                "end_date": j["end_date"].as_str()?,
                "status": j["status"].as_str()?,
            }))
        })
        .collect();
    storage::transaction(&[
        storage::Statement::new(
            "DELETE FROM jobs WHERE item_id IN \
             (SELECT item_id FROM blueprints WHERE owner_kind = $1 AND owner_id = $2)",
            vec![owner.kind.into(), owner.id.into()],
        ),
        storage::Statement::new(
            "INSERT INTO jobs (job_id, item_id, activity, installer_id, runs, start_date, \
                 end_date, status) \
             SELECT DISTINCT ON (x.item_id) x.job_id, x.item_id, x.activity, x.installer_id, \
                 x.runs, x.start_date, x.end_date, x.status \
             FROM json_to_recordset($1::json) AS x(job_id bigint, item_id bigint, \
                 activity integer, installer_id bigint, runs integer, start_date timestamptz, \
                 end_date timestamptz, status text) \
             JOIN blueprints b ON b.item_id = x.item_id \
                 AND b.owner_kind = $2 AND b.owner_id = $3 \
             ORDER BY x.item_id, x.start_date DESC \
             ON CONFLICT (job_id) DO NOTHING",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                owner.kind.into(),
                owner.id.into(),
            ],
        ),
    ])
    .map_err(|e| retry("storing jobs", e))?;
    Ok(())
}

// ---- places ----------------------------------------------------------------

pub fn is_station(id: i64) -> bool {
    (60_000_000..64_000_000).contains(&id)
}

pub fn is_system(id: i64) -> bool {
    (30_000_000..33_000_000).contains(&id)
}

pub fn is_structure(id: i64) -> bool {
    id > 1_000_000_000_000
}

/// A place by itself: a station, structure or system, not an item.
fn is_place(id: i64) -> bool {
    is_station(id) || is_system(id) || is_structure(id)
}

pub fn places() -> Result<(), JobError> {
    let (owners, _lapsed) = owners()?;
    let mut problems = Vec::new();
    for owner in &owners {
        let found = if owner.kind == "corporation" {
            corporate_places(owner)
        } else {
            personal_places(owner)
        };
        match found {
            Ok(found) => store_places(owner, &found)?,
            Err(err) => problems.push(format!(
                "{} {}: places not read: {err:?}",
                owner.kind, owner.id
            )),
        }
    }
    name_places(&owners)?;
    learn_names()?;
    set_error("places_at", &problems)
}

/// For each blueprint: the place at the top, and the containers and
/// hangars between ([type_id, flag], innermost first).
type Found = HashMap<i64, (i64, serde_json::Value)>;

fn owner_items(owner: &Owner) -> Result<Vec<(i64, i64)>, JobError> {
    let rows = storage::query(
        "SELECT item_id, location_id FROM blueprints WHERE owner_kind = $1 AND owner_id = $2",
        &[owner.kind.into(), owner.id.into()],
    )
    .map_err(|e| retry("reading blueprints", e))?;
    Ok(rows.rows.iter().map(|r| (int(r, 0), int(r, 1))).collect())
}

fn corporate_places(owner: &Owner) -> Result<Found, esi::Error> {
    let items = owner_items(owner).unwrap_or_default();
    let mut found = Found::new();
    // Blueprints straight in a station, structure or system need no
    // asking.
    let mut ask = Vec::new();
    for (item, location) in &items {
        if is_place(*location) {
            found.insert(*item, (*location, serde_json::json!([])));
        } else {
            ask.push(*item);
        }
    }
    for chunk in ask.chunks(PLACE_IDS_PER_CALL) {
        let ids = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let answer = esi::get(
            "corporation-asset-places",
            owner.subject,
            &[("item_ids".to_owned(), ids)],
            None,
        )?;
        let list: Vec<serde_json::Value> = serde_json::from_str(&answer.body).unwrap_or_default();
        for place in list {
            let (Some(item), Some(at)) = (place["item_id"].as_i64(), place["place_id"].as_i64())
            else {
                continue;
            };
            let within: Vec<serde_json::Value> = place["within"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|w| serde_json::json!([w["type_id"], w["location_flag"]]))
                .collect();
            found.insert(item, (at, serde_json::Value::Array(within)));
        }
    }
    Ok(found)
}

/// A personal owner's assets are their own: read whole, and each
/// blueprint walked up through its containers.
fn personal_places(owner: &Owner) -> Result<Found, esi::Error> {
    let items = owner_items(owner).unwrap_or_default();
    let assets = pages(esi::get_all("character-assets", owner.subject, &[])?);
    let by_id: HashMap<i64, &serde_json::Value> = assets
        .iter()
        .filter_map(|a| Some((a["item_id"].as_i64()?, a)))
        .collect();
    let mut found = Found::new();
    for (item, location) in items {
        let mut at = location;
        let mut within = Vec::new();
        while let Some(holder) = by_id.get(&at) {
            if within.len() >= 10 {
                break;
            }
            within.push(serde_json::json!([
                holder["type_id"],
                holder["location_flag"]
            ]));
            at = holder["location_id"].as_i64().unwrap_or(0);
        }
        found.insert(item, (at, serde_json::Value::Array(within)));
    }
    Ok(found)
}

fn store_places(owner: &Owner, found: &Found) -> Result<(), JobError> {
    let rows: Vec<serde_json::Value> = found
        .iter()
        .map(|(item, (place, within))| {
            serde_json::json!({ "item_id": item, "place_id": place, "within": within })
        })
        .collect();
    storage::execute(
        "UPDATE blueprints b SET place_id = x.place_id, within = x.within \
         FROM json_to_recordset($1::json) AS x(item_id bigint, place_id bigint, within jsonb) \
         WHERE b.item_id = x.item_id AND b.owner_kind = $2 AND b.owner_id = $3",
        &[
            Db::json(serde_json::Value::Array(rows).to_string()),
            owner.kind.into(),
            owner.id.into(),
        ],
    )
    .map_err(|e| retry("storing places", e))?;
    Ok(())
}

/// Names for places not named yet, named over a week ago, or whose name
/// couldn't be read (within the hour): stations publicly, systems by
/// name, structures through each owner with blueprints there in turn
/// (ESI names a structure only to a character that may dock there),
/// with why not in the app's log.
fn name_places(owners: &[Owner]) -> Result<(), JobError> {
    let due = storage::query(
        "SELECT b.place_id, array_agg(DISTINCT b.owner_kind || ':' || b.owner_id)::text \
         FROM blueprints b LEFT JOIN places p ON p.id = b.place_id \
         WHERE b.place_id IS NOT NULL \
           AND (p.id IS NULL OR p.read_at < now() - interval '7 days' \
                OR (NOT p.named AND p.read_at < now() - interval '1 hour')) \
         GROUP BY b.place_id LIMIT $1",
        &[PLACES_PER_RUN.into()],
    )
    .map_err(|e| retry("finding places", e))?;
    for row in &due.rows {
        let id = int(row, 0);
        // `{corporation:98000001,character:9...}` from Postgres.
        let holders: Vec<&Owner> = text(row, 1)
            .trim_matches(|c| c == '{' || c == '}')
            .split(',')
            .filter_map(|h| {
                let (kind, owner) = h.trim_matches('"').split_once(':')?;
                let owner: i64 = owner.parse().ok()?;
                owners.iter().find(|o| o.kind == kind && o.id == owner)
            })
            .collect();
        let named = if is_station(id) {
            match esi::get(
                "universe-station",
                PUBLIC,
                &[("station_id".to_owned(), id.to_string())],
                None,
            ) {
                Ok(answer) => Some(place_named(&answer.body)),
                Err(err) => {
                    log::info(format!("station {id} not named: {err:?}"));
                    continue;
                }
            }
        } else if is_structure(id) {
            let mut found = None;
            let mut why = Vec::new();
            for owner in &holders {
                let endpoint = if owner.kind == "corporation" {
                    "source-structure"
                } else {
                    "universe-structure"
                };
                match esi::get(
                    endpoint,
                    owner.subject,
                    &[("structure_id".to_owned(), id.to_string())],
                    None,
                ) {
                    Ok(answer) => {
                        found = Some(place_named(&answer.body));
                        break;
                    }
                    Err(err) => why.push(format!("{} {}: {err:?}", owner.kind, owner.id)),
                }
            }
            if found.is_none() {
                log::warn(format!(
                    "structure {id} not named (ESI names it only to a character that may dock \
                     there): {}",
                    if why.is_empty() {
                        "no owner with blueprints there is in use".to_owned()
                    } else {
                        why.join("; ")
                    }
                ));
            }
            found
        } else if is_system(id) {
            esi::names(&[id])
                .ok()
                .and_then(|named| named.into_iter().next())
                .map(|n| (n.name.clone(), n.name))
        } else {
            // Somewhere ESI doesn't say (an item in another's hangar).
            None
        };
        match named {
            Some((name, system)) if !name.is_empty() => store_place(id, &name, &system, true)?,
            _ => {
                let placeholder = if is_structure(id) {
                    format!("Structure {id}")
                } else {
                    format!("Location {id}")
                };
                store_place(id, &placeholder, "", false)?;
            }
        }
    }
    Ok(())
}

/// A station's or structure's name and its system's, from ESI's answer.
fn place_named(body: &str) -> (String, String) {
    let place: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let system = place["system_id"]
        .as_i64()
        .or_else(|| place["solar_system_id"].as_i64())
        .and_then(|s| esi::names(&[s]).ok())
        .and_then(|named| named.into_iter().next())
        .map(|n| n.name)
        .unwrap_or_default();
    let name: String = place["name"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    (name, system)
}

fn store_place(id: i64, name: &str, system: &str, named: bool) -> Result<(), JobError> {
    storage::execute(
        "INSERT INTO places (id, name, system_name, named) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
             system_name = EXCLUDED.system_name, named = EXCLUDED.named, read_at = now()",
        &[id.into(), name.into(), system.into(), named.into()],
    )
    .map_err(|e| retry("storing a place", e))?;
    Ok(())
}

// ---- names -----------------------------------------------------------------

/// Names for the types, owners' corporations, installers and containers
/// not named yet.
fn learn_names() -> Result<(), JobError> {
    let wanted = storage::query(
        "SELECT DISTINCT id FROM ( \
             SELECT type_id AS id FROM blueprints \
             UNION SELECT id FROM owners WHERE kind = 'corporation' \
             UNION SELECT installer_id FROM jobs \
             UNION SELECT (w ->> 0)::bigint FROM blueprints, \
                 jsonb_array_elements(coalesce(within, '[]')) w \
             UNION SELECT product_type_id FROM products) x \
         WHERE id IS NOT NULL AND id > 0 AND id < 1000000000000 \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = x.id) LIMIT 2000",
        &[],
    )
    .map_err(|e| retry("finding names", e))?;
    let ids: Vec<i64> = wanted.rows.iter().map(|r| int(r, 0)).collect();
    for chunk in ids.chunks(1000) {
        match esi::names(chunk) {
            Ok(named) => {
                let rows: Vec<serde_json::Value> = named
                    .into_iter()
                    .map(|n| serde_json::json!({ "id": n.id, "name": n.name }))
                    .collect();
                storage::execute(
                    "INSERT INTO names (id, name) \
                     SELECT id, name FROM json_to_recordset($1::json) AS x(id bigint, name text) \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                    &[Db::json(serde_json::Value::Array(rows).to_string())],
                )
                .map_err(|e| retry("storing names", e))?;
            }
            Err(err) => log::info(format!("names not read: {err:?}")),
        }
    }
    storage::execute(
        "UPDATE owners o SET name = n.name FROM names n \
         WHERE o.kind = 'corporation' AND n.id = o.id AND o.name IS DISTINCT FROM n.name",
        &[],
    )
    .map_err(|e| retry("naming owners", e))?;
    Ok(())
}

/// What a blueprint's name says it makes: "Rifter Blueprint" makes a
/// Rifter, "Fullerides Reaction Formula" Fullerides.
pub fn product_name(blueprint: &str) -> Option<&str> {
    [" Reaction Formula", " Blueprint", " Formula"]
        .iter()
        .find_map(|suffix| blueprint.strip_suffix(suffix))
        .filter(|name| !name.is_empty())
}

/// Each new blueprint type's product, found by name, for its icon.
fn learn_products() -> Result<(), JobError> {
    let wanted = storage::query(
        "SELECT DISTINCT b.type_id, n.name FROM blueprints b JOIN names n ON n.id = b.type_id \
         WHERE NOT EXISTS (SELECT 1 FROM products p WHERE p.blueprint_type_id = b.type_id) \
         LIMIT 500",
        &[],
    )
    .map_err(|e| retry("finding products", e))?;
    if wanted.rows.is_empty() {
        return Ok(());
    }
    let pairs: Vec<(i64, String)> = wanted
        .rows
        .iter()
        .filter_map(|r| Some((int(r, 0), product_name(&text(r, 1))?.to_owned())))
        .collect();
    let lines: Vec<&str> = pairs.iter().map(|(_, n)| n.as_str()).collect();
    let found: HashMap<String, i64> = if lines.is_empty() {
        HashMap::new()
    } else {
        match esi::get(
            "universe-ids",
            PUBLIC,
            &[("names".to_owned(), lines.join("\n"))],
            None,
        ) {
            Ok(answer) => serde_json::from_str::<serde_json::Value>(&answer.body)
                .ok()
                .and_then(|v| v["inventory_types"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| Some((t["name"].as_str()?.to_owned(), t["id"].as_i64()?)))
                .collect(),
            Err(err) => {
                log::info(format!("products not read: {err:?}"));
                return Ok(());
            }
        }
    };
    let rows: Vec<serde_json::Value> = wanted
        .rows
        .iter()
        .map(|r| {
            let product = product_name(&text(r, 1))
                .and_then(|n| found.get(n))
                .copied();
            serde_json::json!({ "blueprint": int(r, 0), "product": product })
        })
        .collect();
    storage::execute(
        "INSERT INTO products (blueprint_type_id, product_type_id) \
         SELECT blueprint, product FROM json_to_recordset($1::json) AS x(blueprint bigint, product bigint) \
         ON CONFLICT (blueprint_type_id) DO NOTHING",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map_err(|e| retry("storing products", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blueprints_name_says_its_product() {
        assert_eq!(product_name("Rifter Blueprint"), Some("Rifter"));
        assert_eq!(
            product_name("Fullerides Reaction Formula"),
            Some("Fullerides")
        );
        assert_eq!(product_name("Blueprint"), None);
        assert_eq!(product_name("Rifter"), None);
    }

    #[test]
    fn places_by_id() {
        assert!(is_station(60003760));
        assert!(is_system(30000142));
        assert!(is_structure(1035466617946));
        assert!(!is_place(1_000_000_123));
    }
}
