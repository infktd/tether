//! Tether's built-in static data (the catalogue's `sde-*` endpoints):
//! item types by name or id, reprocessing materials, groups, market
//! groups and solar systems.

use std::collections::HashMap;

use serde_json::Value;
use tether_plugin_sdk::esi::{self, Error};

use crate::PUBLIC;
use crate::pricing::Item;

/// Ids or names one call may ask for.
const CHUNK: usize = 1000;

/// An item type, as `sde-types` describes it.
#[derive(Debug, Clone, Default)]
pub struct TypeInfo {
    pub id: i64,
    pub name: String,
    pub published: bool,
    pub group_id: i64,
    pub group_name: String,
    pub category_id: i64,
    pub category_name: String,
    pub market_group_id: Option<i64>,
    pub market_group_chain: Vec<i64>,
    pub volume: f64,
    pub packaged_volume: f64,
    pub portion_size: i64,
    pub meta_level: Option<i64>,
    pub compressed_type_id: Option<i64>,
}

fn type_info(v: &Value) -> Option<TypeInfo> {
    Some(TypeInfo {
        id: v["type_id"].as_i64()?,
        name: v["name"].as_str()?.to_owned(),
        published: v["published"].as_bool().unwrap_or(false),
        group_id: v["group_id"].as_i64().unwrap_or_default(),
        group_name: v["group_name"].as_str().unwrap_or_default().to_owned(),
        category_id: v["category_id"].as_i64().unwrap_or_default(),
        category_name: v["category_name"].as_str().unwrap_or_default().to_owned(),
        market_group_id: v["market_group_id"].as_i64(),
        market_group_chain: v["market_group_chain"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        volume: v["volume"].as_f64().unwrap_or_default(),
        packaged_volume: v["packaged_volume"].as_f64().unwrap_or_default(),
        portion_size: v["portion_size"].as_i64().unwrap_or(1),
        meta_level: v["meta_level"].as_i64(),
        compressed_type_id: v["compressed_type_id"].as_i64(),
    })
}

fn ask(endpoint: &str, params: &[(String, String)]) -> Result<Vec<Value>, Error> {
    let body = esi::get(endpoint, PUBLIC, params, None)?.body;
    Ok(serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default())
}

fn id_param(ids: &[i64]) -> (String, String) {
    ("ids".to_owned(), crate::id_list(ids))
}

/// Types by exact name, as the game writes them.
pub fn by_names(names: &[String]) -> Result<HashMap<String, TypeInfo>, Error> {
    let mut unique: Vec<&String> = names.iter().collect();
    unique.sort();
    unique.dedup();
    let mut found = HashMap::new();
    for chunk in unique.chunks(CHUNK) {
        let params: Vec<(String, String)> = chunk
            .iter()
            .map(|n| ("name".to_owned(), (*n).clone()))
            .collect();
        for t in ask("sde-types", &params)?.iter().filter_map(type_info) {
            found.insert(t.name.clone(), t);
        }
    }
    Ok(found)
}

/// Types by id.
pub fn by_ids(ids: &[i64]) -> Result<HashMap<i64, TypeInfo>, Error> {
    let mut unique = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut found = HashMap::new();
    for chunk in unique.chunks(CHUNK) {
        for t in ask("sde-types", &[id_param(chunk)])?
            .iter()
            .filter_map(type_info)
        {
            found.insert(t.id, t);
        }
    }
    Ok(found)
}

/// What one portion of each type reprocesses into.
pub fn materials(ids: &[i64]) -> Result<HashMap<i64, Vec<(i64, i64)>>, Error> {
    let mut unique = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut found = HashMap::new();
    for chunk in unique.chunks(CHUNK) {
        for t in ask("sde-materials", &[id_param(chunk)])? {
            let Some(id) = t["type_id"].as_i64() else {
                continue;
            };
            let list = t["materials"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| Some((m["type_id"].as_i64()?, m["quantity"].as_i64()?)))
                        .collect()
                })
                .unwrap_or_default();
            found.insert(id, list);
        }
    }
    Ok(found)
}

/// Published types whose name contains `q`, blueprints left out.
pub fn search_types(q: &str) -> Result<Vec<TypeInfo>, Error> {
    Ok(ask(
        "sde-type-search",
        &[
            ("q".to_owned(), q.to_owned()),
            ("exclude_category".to_owned(), "9".to_owned()),
            ("limit".to_owned(), "25".to_owned()),
        ],
    )?
    .iter()
    .filter_map(type_info)
    .collect())
}

/// A market group or inventory group: its id and how it's named
/// (market groups by their path, AA's "grandparent -> parent -> name").
#[derive(Debug, Clone)]
pub struct Named {
    pub id: i64,
    pub name: String,
}

fn named(rows: Vec<Value>, id_key: &str, name_key: &str) -> Vec<Named> {
    rows.iter()
        .filter_map(|r| {
            Some(Named {
                id: r[id_key].as_i64()?,
                name: r[name_key].as_str()?.to_owned(),
            })
        })
        .collect()
}

pub fn market_groups(ids: &[i64]) -> Result<Vec<Named>, Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(named(
        ask("sde-market-groups", &[id_param(ids)])?,
        "market_group_id",
        "path",
    ))
}

pub fn search_market_groups(q: &str) -> Result<Vec<Named>, Error> {
    Ok(named(
        ask("sde-market-groups", &[("q".to_owned(), q.to_owned())])?,
        "market_group_id",
        "path",
    ))
}

pub fn groups(ids: &[i64]) -> Result<Vec<Named>, Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(named(
        ask("sde-groups", &[id_param(ids)])?,
        "group_id",
        "name",
    ))
}

pub fn search_groups(q: &str) -> Result<Vec<Named>, Error> {
    Ok(named(
        ask("sde-groups", &[("q".to_owned(), q.to_owned())])?,
        "group_id",
        "name",
    ))
}

pub fn systems(ids: &[i64]) -> Result<Vec<Named>, Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(named(
        ask("sde-systems", &[id_param(ids)])?,
        "system_id",
        "name",
    ))
}

pub fn search_systems(q: &str) -> Result<Vec<Named>, Error> {
    Ok(named(
        ask("sde-systems", &[("q".to_owned(), q.to_owned())])?,
        "system_id",
        "name",
    ))
}

/// The pricing's view of a type.
pub fn item(
    info: &TypeInfo,
    materials: &HashMap<i64, Vec<(i64, i64)>>,
    compressed: &HashMap<i64, TypeInfo>,
) -> Item {
    Item {
        type_id: info.id,
        name: info.name.clone(),
        published: info.published,
        group_id: info.group_id,
        category_name: info.category_name.clone(),
        market_group_chain: info.market_group_chain.clone(),
        packaged_volume: info.packaged_volume,
        portion_size: info.portion_size,
        meta_level: info.meta_level,
        compressed: info
            .compressed_type_id
            .and_then(|c| compressed.get(&c))
            .map(|c| (c.id, c.volume)),
        materials: materials.get(&info.id).cloned().unwrap_or_default(),
    }
}
