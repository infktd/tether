//! Item types from Tether's built-in copy of CCP's static data (the
//! catalogue's `sde-*` endpoints: no token, no ESI request): skills and
//! ships by name, and names by id.

use std::collections::BTreeMap;

use tether_plugin_sdk::esi::{self, Subject};
use tether_plugin_sdk::storage::{self, Value as Db};

/// EVE's category of skills.
pub(crate) const SKILLS: i64 = 16;
/// EVE's category of ships.
pub(crate) const SHIPS: i64 = 6;

/// The static data reads no one's data.
const PUBLIC: Subject = Subject::Character(0);
/// Names one call takes (the catalogue's limit).
const CHUNK: usize = 1000;
/// Names not written as EVE writes them looked up one by one, at most,
/// so one form stays within its calls.
const MAX_SEARCHES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Type {
    pub id: i64,
    /// As EVE writes it.
    pub name: String,
    pub category_id: i64,
}

fn parse(body: &str) -> Vec<Type> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            Some(Type {
                id: t["type_id"].as_i64()?,
                name: t["name"].as_str()?.to_owned(),
                category_id: t["category_id"].as_i64().unwrap_or_default(),
            })
        })
        .collect()
}

fn ask(endpoint: &str, params: &[(String, String)]) -> Result<Vec<Type>, String> {
    esi::get(endpoint, PUBLIC, params, None)
        .map(|answer| parse(&answer.body))
        .map_err(|e| format!("reading Tether's item data: {}", esi::describe(&e)))
}

/// The types named in `names`, by each name as given, lowercased and
/// trimmed: exactly as EVE writes them, else any case. A name that isn't
/// a type is left out.
pub(crate) fn by_names(names: &[&str]) -> Result<BTreeMap<String, Type>, String> {
    let mut wanted: Vec<&str> = names.iter().map(|n| n.trim()).collect();
    wanted.retain(|n| !n.is_empty());
    wanted.sort_unstable();
    wanted.dedup();
    let mut found: BTreeMap<String, Type> = BTreeMap::new();
    for chunk in wanted.chunks(CHUNK) {
        let params: Vec<(String, String)> = chunk
            .iter()
            .map(|n| ("name".to_owned(), (*n).to_owned()))
            .collect();
        for t in ask("sde-types", &params)? {
            found.insert(t.name.to_lowercase(), t);
        }
    }
    let missing: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|n| !found.contains_key(&n.to_lowercase()))
        .collect();
    for name in missing.into_iter().take(MAX_SEARCHES) {
        let lower = name.to_lowercase();
        let hits = ask(
            "sde-type-search",
            &[
                ("q".to_owned(), name.to_owned()),
                ("limit".to_owned(), "10".to_owned()),
            ],
        )?;
        if let Some(t) = hits.into_iter().find(|t| t.name.to_lowercase() == lower) {
            found.insert(lower, t);
        }
    }
    Ok(found)
}

/// Keeps the types' names, so pages name them like the rest.
pub(crate) fn remember<'a>(types: impl IntoIterator<Item = &'a Type>) -> Result<(), String> {
    let rows: Vec<serde_json::Value> = types
        .into_iter()
        .map(|t| serde_json::json!({ "id": t.id, "name": t.name }))
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    storage::execute(
        "INSERT INTO names (id, name, category) \
         SELECT id, name, 'inventory_type' FROM json_to_recordset($1::json) AS x(id bigint, name text) \
         ON CONFLICT (id) DO NOTHING",
        &[Db::json(serde_json::Value::Array(rows).to_string())],
    )
    .map(|_| ())
    .map_err(|e| format!("storing names: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogues_types_are_read() {
        let body = r#"[{"type_id": 3300, "name": "Gunnery", "category_id": 16},
                       {"type_id": 1, "name": null}]"#;
        assert_eq!(
            parse(body),
            vec![Type {
                id: 3300,
                name: "Gunnery".to_owned(),
                category_id: SKILLS
            }]
        );
        assert!(parse("nope").is_empty());
    }
}
