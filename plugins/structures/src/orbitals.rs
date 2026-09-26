//! Starbases, customs offices and Orbital Skyhooks (aa-structures'
//! Starbases and Orbitals), and what the corporation's assets say about
//! Upwell structures: fittings, quantum cores and a Metenox's magmatic gas.
//!
//! - Starbases: ESI's list, then each one's fuel bay. Fuel blocks last
//!   40, 20 or 10 an hour for a large, medium or small tower, a quarter
//!   less where the owner's alliance holds sovereignty (as aa-structures).
//! - Customs offices: ESI's list; the asset name ("Customs Office
//!   (planet)") says which planet.
//! - Skyhooks: ESI lists none but in the corporation's assets, with no
//!   state or fuel (they burn none). Their planet is the nearest one.
//! - A Metenox burns fuel blocks (ESI's fuel_expires) and magmatic gas
//!   (read from its fuel bay): its fuel runs out with the first to go.
//!
//! Starbases, customs offices and assets need the owner character's
//! Director role; an owner without it is backed off like any other.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::log;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};

use crate::notification::{self, Category};
use crate::routing::Routes;
use crate::{
    Budget, Outcome, call, concat, id_list, int, names_for, parse_time, retry, rfc3339,
    seen_and_timers, settings, text, when,
};

/// The Orbital Skyhook's type (the host passes only these, in space).
const SKYHOOK_TYPE: i64 = 81080;
/// A Metenox burns this much magmatic gas an hour (4,800 a day).
pub const METENOX_GAS_PER_HOUR: f64 = 200.0;
/// The Metenox's type name.
const METENOX: &str = "Metenox Moon Drill";
/// How often sovereignty is read (ESI caches it for an hour).
const SOVEREIGNTY_EVERY: &str = "6 hours";
/// Ids per asset names or locations call (the host's limit).
const IDS_PER_CALL: usize = 1000;

/// A public endpoint's subject: not used.
const PUBLIC: Subject = Subject::Character(0);

fn outcome_ok() -> Outcome {
    Outcome::Ok(Vec::new())
}

fn items(bodies: &[String]) -> Vec<serde_json::Value> {
    serde_json::from_str(&concat(bodies)).unwrap_or_default()
}

/// Names the corporation gave its items (a starbase's, a customs
/// office's "Customs Office (planet)"). Optional: trouble is logged, and
/// the structures keep the names they had.
fn asset_names(budget: &mut Budget, owner: i64, ids: &[i64]) -> Vec<(i64, String)> {
    let mut names = Vec::new();
    for chunk in ids.chunks(IDS_PER_CALL) {
        if !budget.take() {
            break;
        }
        match esi::get(
            "corporation-asset-names",
            Subject::DataSource(owner),
            &[("item_ids".to_owned(), id_list(chunk))],
            None,
        ) {
            Ok(response) => {
                #[derive(Deserialize)]
                struct Named {
                    item_id: i64,
                    name: String,
                }
                let named: Vec<Named> = serde_json::from_str(&response.body).unwrap_or_default();
                names.extend(named.into_iter().map(|n| (n.item_id, n.name)));
            }
            Err(err) => {
                log::info(format!("asset names for owner {owner}: {err:?}"));
                break;
            }
        }
    }
    names
}

fn name_of(names: &[(i64, String)], id: i64) -> Option<String> {
    names
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| n.trim().to_owned())
        .filter(|n| !n.is_empty() && n != "None")
}

/// "Customs Office (Jita IV)" into "Jita IV".
pub fn office_planet(name: &str) -> Option<&str> {
    name.strip_prefix("Customs Office (")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

#[derive(Deserialize)]
struct Starbase {
    starbase_id: i64,
    type_id: i64,
    system_id: i64,
    #[serde(default)]
    moon_id: Option<i64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    reinforced_until: Option<String>,
    #[serde(default)]
    unanchor_at: Option<String>,
    #[serde(default)]
    onlined_since: Option<String>,
}

/// The corporation's starbases, replaced whole, with their fuel bays
/// (those the budget allows this run; the rest keep what they had).
pub fn read_starbases(budget: &mut Budget, corp: i64, owner: i64) -> Result<Outcome, JobError> {
    let bodies = match call(
        budget,
        "corporation-starbases",
        Subject::DataSource(owner),
        &[],
        true,
    ) {
        Outcome::Ok(bodies) => bodies,
        other => return Ok(other),
    };
    let starbases: Vec<Starbase> = serde_json::from_str(&concat(&bodies)).unwrap_or_default();
    let mut fuels: Vec<(i64, serde_json::Value)> = Vec::new();
    for s in &starbases {
        if !budget.take() {
            log::info("starbase fuel: out of ESI calls this run");
            break;
        }
        match esi::get(
            "corporation-starbase",
            Subject::DataSource(owner),
            &[
                ("starbase_id".to_owned(), s.starbase_id.to_string()),
                ("system_id".to_owned(), s.system_id.to_string()),
            ],
            None,
        ) {
            Ok(response) => {
                let body: serde_json::Value =
                    serde_json::from_str(&response.body).unwrap_or_default();
                fuels.push((s.starbase_id, body["fuels"].clone()));
            }
            Err(err) => log::info(format!("starbase {} fuel: {err:?}", s.starbase_id)),
        }
    }
    let ids: Vec<i64> = starbases.iter().map(|s| s.starbase_id).collect();
    let names = asset_names(budget, owner, &ids);
    let rows: Vec<serde_json::Value> = starbases
        .iter()
        .map(|s| {
            let fuel = fuels
                .iter()
                .find(|(id, _)| *id == s.starbase_id)
                .map(|(_, f)| f.clone())
                .filter(serde_json::Value::is_array);
            serde_json::json!({
                "structure_id": s.starbase_id,
                "type_id": s.type_id,
                "system_id": s.system_id,
                "moon_id": s.moon_id,
                "state": s.state.clone().unwrap_or_else(|| "unknown".to_owned()),
                "state_timer_end": s.reinforced_until.as_deref().and_then(parse_time).map(rfc3339),
                "unanchors_at": s.unanchor_at.as_deref().and_then(parse_time).map(rfc3339),
                "onlined_since": s.onlined_since.as_deref().and_then(parse_time).map(rfc3339),
                "name": name_of(&names, s.starbase_id),
                "fuels": fuel,
            })
        })
        .collect();
    let mut statements = vec![
        Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, kind, name, type_id, system_id, moon_id, \
                 state, state_timer_end, unanchors_at, onlined_since, details, fuel_read_at, updated_at) \
             SELECT structure_id, $2, 'starbase', coalesce(name, 'Starbase'), type_id, system_id, moon_id, \
                 state, state_timer_end, unanchors_at, onlined_since, \
                 CASE WHEN fuels IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('fuels', fuels) END, \
                 CASE WHEN fuels IS NULL THEN NULL ELSE now() END, now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, type_id bigint, system_id bigint, \
                 moon_id bigint, state text, state_timer_end timestamptz, unanchors_at timestamptz, \
                 onlined_since timestamptz, name text, fuels jsonb) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                 kind = 'starbase', type_id = EXCLUDED.type_id, system_id = EXCLUDED.system_id, \
                 moon_id = EXCLUDED.moon_id, state = EXCLUDED.state, \
                 state_timer_end = EXCLUDED.state_timer_end, unanchors_at = EXCLUDED.unanchors_at, \
                 onlined_since = EXCLUDED.onlined_since, \
                 name = CASE WHEN EXCLUDED.name = 'Starbase' THEN structures.name ELSE EXCLUDED.name END, \
                 details = CASE WHEN EXCLUDED.fuel_read_at IS NULL THEN structures.details ELSE EXCLUDED.details END, \
                 fuel_read_at = coalesce(EXCLUDED.fuel_read_at, structures.fuel_read_at), \
                 updated_at = now()",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                corp.into(),
            ],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id = $1 AND kind = 'starbase' AND updated_at < now()",
            vec![corp.into()],
        ),
    ];
    statements.extend(seen_and_timers(corp));
    storage::transaction(&statements).map_err(|e| retry("storing starbases", e))?;
    Ok(outcome_ok())
}

/// The corporation's customs offices, replaced whole: reinforcement
/// window, access and taxes as ESI gives them, and the planet from the
/// asset name.
pub fn read_offices(budget: &mut Budget, corp: i64, owner: i64) -> Result<Outcome, JobError> {
    let bodies = match call(
        budget,
        "corporation-customs-offices",
        Subject::DataSource(owner),
        &[],
        true,
    ) {
        Outcome::Ok(bodies) => bodies,
        other => return Ok(other),
    };
    let offices = items(&bodies);
    let ids: Vec<i64> = offices
        .iter()
        .filter_map(|o| o["office_id"].as_i64())
        .collect();
    let names = asset_names(budget, owner, &ids);
    let rows: Vec<serde_json::Value> = offices
        .iter()
        .filter_map(|o| {
            let id = o["office_id"].as_i64()?;
            let name = name_of(&names, id);
            let planet = name.as_deref().and_then(office_planet).map(str::to_owned);
            Some(serde_json::json!({
                "structure_id": id,
                "type_id": o["type_id"].as_i64().unwrap_or(2233),
                "system_id": o["system_id"].as_i64()?,
                "reinforce_hour": o["reinforce_exit_start"].as_i64(),
                "name": name,
                "planet_name": planet,
                "details": o,
            }))
        })
        .collect();
    let mut statements = vec![
        Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, kind, name, type_id, system_id, state, \
                 reinforce_hour, planet_name, details, updated_at) \
             SELECT structure_id, $2, 'customs_office', coalesce(name, 'Customs Office'), type_id, system_id, \
                 'none', reinforce_hour, planet_name, details, now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, type_id bigint, system_id bigint, \
                 reinforce_hour integer, name text, planet_name text, details jsonb) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                 kind = 'customs_office', type_id = EXCLUDED.type_id, system_id = EXCLUDED.system_id, \
                 reinforce_hour = EXCLUDED.reinforce_hour, details = EXCLUDED.details, \
                 name = CASE WHEN EXCLUDED.name = 'Customs Office' THEN structures.name ELSE EXCLUDED.name END, \
                 planet_name = coalesce(EXCLUDED.planet_name, structures.planet_name), \
                 updated_at = now()",
            vec![
                Db::json(serde_json::Value::Array(rows).to_string()),
                corp.into(),
            ],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id = $1 AND kind = 'customs_office' AND updated_at < now()",
            vec![corp.into()],
        ),
    ];
    statements.extend(seen_and_timers(corp));
    storage::transaction(&statements).map_err(|e| retry("storing customs offices", e))?;
    Ok(outcome_ok())
}

#[derive(Deserialize)]
struct Asset {
    item_id: i64,
    type_id: i64,
    location_id: i64,
    location_flag: String,
    location_type: String,
    quantity: i64,
}

/// What the corporation's assets say (the host passes only structures'
/// slots and bays, and skyhooks): skyhooks, replaced whole; each Upwell
/// structure's items, whether it has a quantum core and a fitting. Only a
/// whole read is stored: until then the last one stands.
pub fn read_assets(budget: &mut Budget, corp: i64, owner: i64) -> Result<Outcome, JobError> {
    let bodies = match call(
        budget,
        "corporation-structure-assets",
        Subject::DataSource(owner),
        &[],
        true,
    ) {
        Outcome::Ok(bodies) => bodies,
        other => return Ok(other),
    };
    let assets: Vec<Asset> = serde_json::from_str(&concat(&bodies)).unwrap_or_default();
    let (skyhooks, slotted): (Vec<&Asset>, Vec<&Asset>) = assets
        .iter()
        .partition(|a| a.type_id == SKYHOOK_TYPE && a.location_type == "solar_system");
    let skyhooks: Vec<serde_json::Value> = skyhooks
        .iter()
        .map(|a| {
            serde_json::json!({
                "structure_id": a.item_id,
                "type_id": a.type_id,
                "system_id": a.location_id,
            })
        })
        .collect();
    let slotted: Vec<serde_json::Value> = slotted
        .iter()
        .map(|a| {
            serde_json::json!({
                "item_id": a.item_id,
                "structure_id": a.location_id,
                "type_id": a.type_id,
                "flag": a.location_flag,
                "quantity": a.quantity,
            })
        })
        .collect();
    let mut statements = vec![
        Statement::new(
            "INSERT INTO structures (structure_id, corporation_id, kind, name, type_id, system_id, state, updated_at) \
             SELECT structure_id, $2, 'skyhook', 'Orbital Skyhook', type_id, system_id, 'none', now() \
             FROM json_to_recordset($1::json) AS x(structure_id bigint, type_id bigint, system_id bigint) \
             ON CONFLICT (structure_id) DO UPDATE SET corporation_id = EXCLUDED.corporation_id, \
                 kind = 'skyhook', type_id = EXCLUDED.type_id, system_id = EXCLUDED.system_id, updated_at = now()",
            vec![
                Db::json(serde_json::Value::Array(skyhooks).to_string()),
                corp.into(),
            ],
        ),
        Statement::new(
            "DELETE FROM structures WHERE corporation_id = $1 AND kind = 'skyhook' AND updated_at < now()",
            vec![corp.into()],
        ),
        Statement::new(
            "DELETE FROM structure_items WHERE corporation_id = $1",
            vec![corp.into()],
        ),
        // Only what's in this corporation's Upwell structures (not its
        // ships' fittings, which share the slots).
        Statement::new(
            "INSERT INTO structure_items (item_id, structure_id, corporation_id, type_id, flag, quantity) \
             SELECT x.item_id, x.structure_id, $2, x.type_id, x.flag, x.quantity \
             FROM json_to_recordset($1::json) AS x(item_id bigint, structure_id bigint, type_id bigint, \
                 flag text, quantity bigint) \
             JOIN structures s ON s.structure_id = x.structure_id AND s.corporation_id = $2 AND s.kind = 'upwell' \
             ON CONFLICT (item_id) DO UPDATE SET structure_id = EXCLUDED.structure_id, \
                 corporation_id = EXCLUDED.corporation_id, type_id = EXCLUDED.type_id, \
                 flag = EXCLUDED.flag, quantity = EXCLUDED.quantity",
            vec![
                Db::json(serde_json::Value::Array(slotted).to_string()),
                corp.into(),
            ],
        ),
        Statement::new(
            "UPDATE structures s SET \
                 has_core = EXISTS (SELECT 1 FROM structure_items i WHERE i.structure_id = s.structure_id \
                     AND i.flag = 'QuantumCoreRoom'), \
                 has_fitting = EXISTS (SELECT 1 FROM structure_items i WHERE i.structure_id = s.structure_id \
                     AND i.flag ~ '^(HiSlot|MedSlot|LoSlot|RigSlot|ServiceSlot)[0-9]$'), \
                 fuel_read_at = now() \
             WHERE s.corporation_id = $1 AND s.kind = 'upwell'",
            vec![corp.into()],
        ),
    ];
    statements.extend(seen_and_timers(corp));
    storage::transaction(&statements).map_err(|e| retry("storing assets", e))?;
    locate_skyhooks(budget, corp, owner)?;
    Ok(outcome_ok())
}

/// Where the corporation's skyhooks are (once: they don't move), to find
/// their planets.
fn locate_skyhooks(budget: &mut Budget, corp: i64, owner: i64) -> Result<(), JobError> {
    let unplaced = storage::query(
        "SELECT structure_id FROM structures WHERE corporation_id = $1 AND kind = 'skyhook' AND x IS NULL \
         ORDER BY structure_id LIMIT 1000",
        &[corp.into()],
    )
    .map_err(|e| retry("finding skyhooks", e))?;
    let ids: Vec<i64> = unplaced.rows.iter().map(|r| int(r, 0)).collect();
    if ids.is_empty() || !budget.take() {
        return Ok(());
    }
    match esi::get(
        "corporation-asset-locations",
        Subject::DataSource(owner),
        &[("item_ids".to_owned(), id_list(&ids))],
        None,
    ) {
        Ok(response) => {
            let located: Vec<serde_json::Value> =
                serde_json::from_str(&response.body).unwrap_or_default();
            let rows: Vec<serde_json::Value> = located
                .iter()
                .filter_map(|l| {
                    Some(serde_json::json!({
                        "item_id": l["item_id"].as_i64()?,
                        "x": l["position"]["x"].as_f64()?,
                        "y": l["position"]["y"].as_f64()?,
                        "z": l["position"]["z"].as_f64()?,
                    }))
                })
                .collect();
            storage::execute(
                "UPDATE structures s SET x = l.x, y = l.y, z = l.z \
                 FROM json_to_recordset($1::json) AS l(item_id bigint, x double precision, \
                     y double precision, z double precision) \
                 WHERE s.structure_id = l.item_id AND s.kind = 'skyhook'",
                &[Db::json(serde_json::Value::Array(rows).to_string())],
            )
            .map_err(|e| retry("storing skyhook positions", e))?;
        }
        Err(err) => log::info(format!("skyhook positions for owner {owner}: {err:?}")),
    }
    Ok(())
}

/// Which alliance holds which system, every few hours while there are
/// structures (for starbases' fuel and the sov tag).
pub fn learn_sovereignty(budget: &mut Budget) -> Result<(), JobError> {
    let due = storage::query(
        &format!(
            "SELECT 1 FROM settings WHERE id = 1 \
               AND (sovereignty_at IS NULL OR sovereignty_at < now() - interval '{SOVEREIGNTY_EVERY}') \
               AND EXISTS (SELECT 1 FROM structures)"
        ),
        &[],
    )
    .map_err(|e| retry("checking sovereignty", e))?;
    if due.rows.is_empty() || !budget.take() {
        return Ok(());
    }
    match esi::get("sovereignty-systems", PUBLIC, &[], None) {
        Ok(response) => {
            storage::transaction(&[
                Statement::new("DELETE FROM sovereignty", vec![]),
                Statement::new(
                    "INSERT INTO sovereignty (system_id, alliance_id) \
                     SELECT DISTINCT ON (system_id) system_id, alliance_id \
                     FROM json_to_recordset($1::json) AS x(system_id bigint, alliance_id bigint) \
                     WHERE system_id IS NOT NULL AND alliance_id IS NOT NULL",
                    vec![Db::json(response.body)],
                ),
                Statement::new(
                    "UPDATE settings SET sovereignty_at = now() WHERE id = 1",
                    vec![],
                ),
            ])
            .map_err(|e| retry("storing sovereignty", e))?;
        }
        Err(err) => log::warn(format!("sovereignty: {err:?}")),
    }
    Ok(())
}

#[derive(Deserialize)]
struct Position {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Deserialize)]
struct Planet {
    planet_id: i64,
    name: String,
    system_id: i64,
    position: Position,
}

/// Planets of systems with customs offices or skyhooks (once each).
pub fn learn_planets(budget: &mut Budget) -> Result<(), JobError> {
    let missing = storage::query(
        "SELECT DISTINCT p.id::bigint FROM systems y \
         CROSS JOIN LATERAL unnest(string_to_array(y.planet_ids, ',')) AS p(id) \
         WHERE y.planet_ids <> '' \
           AND EXISTS (SELECT 1 FROM structures s WHERE s.system_id = y.system_id \
               AND s.kind IN ('customs_office', 'skyhook')) \
           AND NOT EXISTS (SELECT 1 FROM planets q WHERE q.planet_id = p.id::bigint) \
         LIMIT 40",
        &[],
    )
    .map_err(|e| retry("finding planets", e))?;
    for row in &missing.rows {
        let planet = int(row, 0);
        if !budget.take() {
            break;
        }
        match esi::get(
            "universe-planet",
            PUBLIC,
            &[("planet_id".to_owned(), planet.to_string())],
            None,
        ) {
            Ok(response) => {
                let Ok(p) = serde_json::from_str::<Planet>(&response.body) else {
                    log::warn(format!("planet {planet}: unexpected answer"));
                    continue;
                };
                storage::transaction(&[
                    Statement::new(
                        "INSERT INTO planets (planet_id, system_id, name, x, y, z) VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (planet_id) DO NOTHING",
                        vec![
                            p.planet_id.into(),
                            p.system_id.into(),
                            p.name.as_str().into(),
                            p.position.x.into(),
                            p.position.y.into(),
                            p.position.z.into(),
                        ],
                    ),
                    Statement::new(
                        "INSERT INTO names (id, name, category) VALUES ($1, $2, 'planet') \
                         ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                        vec![p.planet_id.into(), p.name.into()],
                    ),
                ])
                .map_err(|e| retry("storing a planet", e))?;
            }
            Err(err) => log::warn(format!("planet {planet}: {err:?}")),
        }
    }
    Ok(())
}

/// Names of starbases' moons (once each).
pub fn learn_moons(budget: &mut Budget) -> Result<(), JobError> {
    let missing = storage::query(
        "SELECT DISTINCT moon_id FROM structures s WHERE kind = 'starbase' AND moon_id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM names n WHERE n.id = s.moon_id) LIMIT 30",
        &[],
    )
    .map_err(|e| retry("finding moons", e))?;
    for row in &missing.rows {
        let moon = int(row, 0);
        if !budget.take() {
            break;
        }
        match esi::get(
            "universe-moon",
            PUBLIC,
            &[("moon_id".to_owned(), moon.to_string())],
            None,
        ) {
            Ok(response) => {
                let body: serde_json::Value =
                    serde_json::from_str(&response.body).unwrap_or_default();
                let Some(name) = body["name"].as_str() else {
                    log::warn(format!("moon {moon}: unexpected answer"));
                    continue;
                };
                storage::execute(
                    "INSERT INTO names (id, name, category) VALUES ($1, $2, 'moon') \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                    &[moon.into(), name.into()],
                )
                .map_err(|e| retry("storing a moon", e))?;
            }
            Err(err) => log::warn(format!("moon {moon}: {err:?}")),
        }
    }
    Ok(())
}

/// Customs offices' planets by name; skyhooks' as the nearest planet, once
/// every planet of the system is known.
pub fn resolve_orbitals() -> Result<(), JobError> {
    storage::transaction(&[
        Statement::new(
            "UPDATE structures s SET planet_id = p.planet_id FROM planets p \
             WHERE s.kind = 'customs_office' AND p.system_id = s.system_id AND p.name = s.planet_name \
               AND s.planet_id IS DISTINCT FROM p.planet_id",
            vec![],
        ),
        Statement::new(
            "UPDATE structures s SET planet_id = n.planet_id, planet_name = n.name, \
                 name = 'Orbital Skyhook (' || n.name || ')' \
             FROM (SELECT DISTINCT ON (k.structure_id) k.structure_id, p.planet_id, p.name \
                   FROM structures k JOIN planets p ON p.system_id = k.system_id \
                   JOIN systems y ON y.system_id = k.system_id AND y.planet_ids IS NOT NULL \
                   WHERE k.kind = 'skyhook' AND k.x IS NOT NULL \
                     AND NOT EXISTS (SELECT 1 FROM unnest(string_to_array(y.planet_ids, ',')) AS q(id) \
                         WHERE q.id <> '' AND NOT EXISTS (SELECT 1 FROM planets z WHERE z.planet_id = q.id::bigint)) \
                   ORDER BY k.structure_id, (p.x - k.x) ^ 2 + (p.y - k.y) ^ 2 + (p.z - k.z) ^ 2) n \
             WHERE s.structure_id = n.structure_id \
               AND (s.planet_id IS DISTINCT FROM n.planet_id OR s.name <> 'Orbital Skyhook (' || n.name || ')')",
            vec![],
        ),
    ])
    .map_err(|e| retry("placing orbitals", e))?;
    Ok(())
}

/// Fuel blocks a tower burns an hour, by its type name (aa-structures:
/// small, medium, else large), a quarter less in its alliance's sov.
pub fn tower_blocks_per_hour(type_name: &str, sov: bool) -> f64 {
    let lower = type_name.to_lowercase();
    let base = if lower.contains("small") {
        10.0
    } else if lower.contains("medium") {
        20.0
    } else {
        40.0
    };
    if sov { base * 0.75 } else { base }
}

/// When a tower's blocks run out, read at `read_at`; none when it's
/// offline or has no blocks.
pub fn tower_fuel_expires(
    read_at: DateTime<Utc>,
    blocks: i64,
    per_hour: f64,
    state: &str,
) -> Option<DateTime<Utc>> {
    if state == "offline" || blocks <= 0 || per_hour <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let seconds = (blocks as f64 * 3600.0 / per_hour).floor() as i64;
    read_at.checked_add_signed(Duration::seconds(seconds))
}

#[derive(Deserialize)]
struct Fuel {
    type_id: i64,
    quantity: i64,
}

/// Starbases' fuel from their fuel bays, and Upwell structures' blocks
/// and a Metenox's magmatic gas from theirs. Type names say which item is
/// which (fuel blocks, strontium, magmatic gas).
pub fn compute_fuel() -> Result<(), JobError> {
    let rows = storage::query(
        "SELECT s.structure_id, coalesce(t.name, ''), s.details::text, s.fuel_read_at, s.state, \
             EXISTS (SELECT 1 FROM sovereignty v JOIN owners o ON o.alliance_id = v.alliance_id \
                 WHERE v.system_id = s.system_id AND o.corporation_id = s.corporation_id) \
         FROM structures s LEFT JOIN names t ON t.id = s.type_id \
         WHERE s.kind = 'starbase' AND s.fuel_read_at IS NOT NULL",
        &[],
    )
    .map_err(|e| retry("reading starbases", e))?;
    let mut parsed = Vec::new();
    let mut ids = Vec::new();
    for row in &rows.rows {
        let details: serde_json::Value = serde_json::from_str(&text(row, 2)).unwrap_or_default();
        let fuels: Vec<Fuel> = serde_json::from_value(details["fuels"].clone()).unwrap_or_default();
        ids.extend(fuels.iter().map(|f| f.type_id));
        parsed.push((row, fuels));
    }
    ids.sort_unstable();
    ids.dedup();
    let names = names_for(&ids).map_err(|e| retry("reading names", e))?;
    let named = |id: i64| {
        names
            .iter()
            .find(|(i, _)| *i == id)
            .map_or("", |(_, n)| n.as_str())
    };
    let mut updates: Vec<serde_json::Value> = Vec::new();
    for (row, fuels) in parsed {
        let (id, type_name, state) = (int(row, 0), text(row, 1), text(row, 4));
        let Some(read_at) = when(row, 3) else {
            continue;
        };
        if type_name.is_empty() {
            continue;
        }
        let sov = row.get(5).and_then(Db::as_bool).unwrap_or(false);
        let blocks: i64 = fuels
            .iter()
            .filter(|f| named(f.type_id).ends_with("Fuel Block"))
            .map(|f| f.quantity)
            .sum();
        let strontium: i64 = fuels
            .iter()
            .filter(|f| named(f.type_id) == "Strontium Clathrates")
            .map(|f| f.quantity)
            .sum();
        let expires = tower_fuel_expires(
            read_at,
            blocks,
            tower_blocks_per_hour(&type_name, sov),
            &state,
        );
        updates.push(serde_json::json!({
            "structure_id": id,
            "blocks": blocks,
            "strontium": strontium,
            "expires": expires.map(rfc3339),
        }));
    }
    storage::transaction(&[
        Statement::new(
            "UPDATE structures s SET fuel_blocks = u.blocks, strontium = u.strontium, \
                 blocks_expires = u.expires, fuel_expires = u.expires \
             FROM json_to_recordset($1::json) AS u(structure_id bigint, blocks bigint, strontium bigint, \
                 expires timestamptz) \
             WHERE s.structure_id = u.structure_id",
            vec![Db::json(serde_json::Value::Array(updates).to_string())],
        ),
        Statement::new(
            format!(
                "UPDATE structures s SET fuel_blocks = f.blocks, magmatic_gas = f.gas, \
                     gas_expires = CASE WHEN f.type_name = '{METENOX}' \
                         THEN s.fuel_read_at + make_interval(secs => (f.gas * 3600.0 / {METENOX_GAS_PER_HOUR})::double precision) END, \
                     fuel_expires = least(s.blocks_expires, CASE WHEN f.type_name = '{METENOX}' \
                         THEN s.fuel_read_at + make_interval(secs => (f.gas * 3600.0 / {METENOX_GAS_PER_HOUR})::double precision) END) \
                 FROM (SELECT k.structure_id, coalesce(t.name, '') AS type_name, \
                           coalesce(sum(i.quantity) FILTER (WHERE n.name LIKE '% Fuel Block'), 0) AS blocks, \
                           coalesce(sum(i.quantity) FILTER (WHERE n.name = 'Magmatic Gas'), 0) AS gas \
                       FROM structures k LEFT JOIN names t ON t.id = k.type_id \
                       LEFT JOIN structure_items i ON i.structure_id = k.structure_id AND i.flag = 'StructureFuel' \
                       LEFT JOIN names n ON n.id = i.type_id \
                       WHERE k.kind = 'upwell' AND k.fuel_read_at IS NOT NULL \
                       GROUP BY k.structure_id, t.name) f \
                 WHERE s.structure_id = f.structure_id"
            ),
            vec![],
        ),
    ])
    .map_err(|e| retry("computing fuel", e))?;
    Ok(())
}

/// aa-structures' "starbase reinforced" (its own notification, from the
/// starbase's state): once per reinforcement, to the owner's attack
/// channel.
pub fn starbase_reinforcements() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let routes = Routes::load(&settings).map_err(|e| retry("reading routes", e))?;
    let rows = storage::query(
        "SELECT s.structure_id, s.corporation_id, s.name, coalesce(t.name, ''), coalesce(m.name, ''), \
             coalesce(y.name, sn.name, ''), s.state_timer_end \
         FROM structures s LEFT JOIN names t ON t.id = s.type_id LEFT JOIN names m ON m.id = s.moon_id \
         LEFT JOIN systems y ON y.system_id = s.system_id LEFT JOIN names sn ON sn.id = s.system_id \
         WHERE s.kind = 'starbase' AND s.state = 'reinforced' AND s.state_timer_end > now() \
           AND s.updated_at > now() - interval '1 day'",
        &[],
    )
    .map_err(|e| retry("reading reinforced starbases", e))?;
    for row in &rows.rows {
        let (id, corp) = (int(row, 0), int(row, 1));
        let Some(until) = when(row, 6) else {
            continue;
        };
        let Some(channel) = routes.channel(corp, Category::Attack) else {
            continue;
        };
        let mut place = notification::escape(&text(row, 2));
        for (i, fmt) in [(3, " ({})"), (4, " at {}"), (5, " in {}")] {
            let value = text(row, i);
            if !value.is_empty() {
                place.push_str(&fmt.replace("{}", &notification::escape(&value)));
            }
        }
        let message = format!(
            "Starbase reinforced: {place}. It comes out of reinforcement {} EVE.",
            until.format("%Y-%m-%d %H:%M")
        );
        storage::execute(
            "INSERT INTO outbox (key, channel, message, mention) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (key) DO NOTHING",
            &[
                format!("pos-reinforced:{id}:{}", until.timestamp()).into(),
                channel.into(),
                message.into(),
                routes.mention(corp).into(),
            ],
        )
        .map_err(|e| retry("queuing a starbase message", e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn towers_burn_by_size_and_sov() {
        assert_eq!(tower_blocks_per_hour("Caldari Control Tower", false), 40.0);
        assert_eq!(
            tower_blocks_per_hour("Caldari Control Tower Medium", false),
            20.0
        );
        assert_eq!(
            tower_blocks_per_hour("Amarr Control Tower Small", true),
            7.5
        );
        let read = DateTime::parse_from_rfc3339("2026-09-26T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            tower_fuel_expires(read, 960, 40.0, "online"),
            Some(read + Duration::hours(24))
        );
        assert_eq!(tower_fuel_expires(read, 960, 40.0, "offline"), None);
        assert_eq!(tower_fuel_expires(read, 0, 40.0, "online"), None);
    }

    #[test]
    fn a_customs_office_is_named_for_its_planet() {
        assert_eq!(office_planet("Customs Office (Jita IV)"), Some("Jita IV"));
        assert_eq!(office_planet("Customs Office"), None);
        assert_eq!(office_planet("My office"), None);
    }
}
