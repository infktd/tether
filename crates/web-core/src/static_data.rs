//! The catalogue's `sde-*` endpoints: Tether's built-in copy of CCP's
//! static data (`tether_sde`), for any app, with no token and no ESI
//! call. Types by id or exact name, searches, reprocessing materials,
//! groups, the market group tree and solar systems.

use serde_json::{Value, json};
use tether_plugins::services::EsiError;
use tether_sde::{Sde, Type, sde};

/// Ids or names one call may ask for.
const MAX_ASKED: usize = 1000;
/// Most results a search answers.
const MAX_FOUND: usize = 50;

/// The answer to `name` if it's one of the `sde-*` endpoints; `None`
/// otherwise (the call goes on to ESI's catalogue).
pub fn get(name: &str, params: &[(String, String)]) -> Option<Result<String, EsiError>> {
    let sde = sde();
    let answer = match name {
        "sde-types" => types(sde, params),
        "sde-type-search" => type_search(sde, params),
        "sde-materials" => materials(sde, params),
        "sde-groups" => groups(sde, params),
        "sde-market-groups" => market_groups(sde, params),
        "sde-systems" => systems(sde, params),
        _ => return None,
    };
    Some(answer.map(|v| v.to_string()))
}

fn invalid(why: impl Into<String>) -> EsiError {
    EsiError::Invalid(why.into())
}

/// The comma-separated positive ids of `key`, at most 1,000.
fn ids(params: &[(String, String)], key: &str) -> Result<Option<Vec<i64>>, EsiError> {
    let Some((_, list)) = params.iter().find(|(k, _)| k == key) else {
        return Ok(None);
    };
    let ids = list
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse::<i64>().ok().filter(|id| *id > 0))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid(format!("{key} are positive numbers, comma-separated")))?;
    if ids.len() > MAX_ASKED {
        return Err(invalid(format!("at most {MAX_ASKED} {key}")));
    }
    Ok(Some(ids))
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn limit(params: &[(String, String)]) -> usize {
    param(params, "limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(MAX_FOUND)
        .clamp(1, MAX_FOUND)
}

fn type_json(sde: &Sde, t: &Type) -> Value {
    let group = sde.group(t.group_id);
    let category_id = group.map(|g| g.category_id);
    let chain: Vec<i64> = t
        .market_group_id
        .map(|m| sde.market_group_chain(m).iter().map(|g| g.id).collect())
        .unwrap_or_default();
    json!({
        "type_id": t.id,
        "name": t.name,
        "published": t.published,
        "group_id": t.group_id,
        "group_name": group.map(|g| g.name.as_str()),
        "category_id": category_id,
        "category_name": category_id.and_then(|c| sde.category_name(c)),
        "market_group_id": t.market_group_id,
        "market_group_chain": chain,
        "volume": t.volume,
        "packaged_volume": t.packaged_volume,
        "portion_size": t.portion_size,
        "meta_level": t.meta_level,
        "compressed_type_id": t.compressed_type_id,
    })
}

/// `ids=`, or one `name=` per exact name (as the game writes it): the
/// types found, each once.
fn types(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let names: Vec<&str> = params
        .iter()
        .filter(|(k, _)| k == "name")
        .map(|(_, v)| v.as_str())
        .collect();
    if names.len() > MAX_ASKED {
        return Err(invalid(format!("at most {MAX_ASKED} names")));
    }
    let mut found: Vec<&Type> = match ids(params, "ids")? {
        Some(ids) => ids.iter().filter_map(|id| sde.type_by_id(*id)).collect(),
        None if !names.is_empty() => names.iter().filter_map(|n| sde.type_by_name(n)).collect(),
        None => return Err(invalid("give ids, or names (one name= each)")),
    };
    found.sort_by_key(|t| t.id);
    found.dedup_by_key(|t| t.id);
    Ok(Value::Array(
        found.iter().map(|t| type_json(sde, t)).collect(),
    ))
}

/// `q=`: published types whose name contains it, names starting with it
/// first; `limit=` (at most 50); `exclude_category=` ids left out (e.g.
/// 9, blueprints).
fn type_search(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let q = param(params, "q").unwrap_or_default();
    let excluded = ids(params, "exclude_category")?.unwrap_or_default();
    let found: Vec<Value> = sde
        .search_types(q, MAX_FOUND * 4)
        .into_iter()
        .filter(|t| {
            sde.group(t.group_id)
                .is_none_or(|g| !excluded.contains(&g.category_id))
        })
        .take(limit(params))
        .map(|t| type_json(sde, t))
        .collect();
    Ok(Value::Array(found))
}

/// `ids=`: what one portion of each type reprocesses into.
fn materials(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let ids = ids(params, "ids")?.ok_or_else(|| invalid("give ids"))?;
    Ok(Value::Array(
        ids.iter()
            .filter_map(|id| sde.type_by_id(*id))
            .map(|t| {
                json!({
                    "type_id": t.id,
                    "portion_size": t.portion_size,
                    "materials": sde.materials(t.id).iter()
                        .map(|m| json!({ "type_id": m.type_id, "quantity": m.quantity }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    ))
}

/// `ids=` or `q=`: inventory groups, with their category.
fn groups(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let found: Vec<&tether_sde::Group> = match ids(params, "ids")? {
        Some(ids) => ids.iter().filter_map(|id| sde.group(*id)).collect(),
        None => sde.search_groups(param(params, "q").unwrap_or_default(), limit(params)),
    };
    Ok(Value::Array(
        found
            .iter()
            .map(|g| {
                json!({
                    "group_id": g.id,
                    "name": g.name,
                    "category_id": g.category_id,
                    "category_name": sde.category_name(g.category_id),
                    "published": g.published,
                })
            })
            .collect(),
    ))
}

/// `ids=` or `q=`: market groups, each with its ancestors' ids (itself
/// first, up to the root) and its path as AA labels it ("grandparent ->
/// parent -> name").
fn market_groups(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let found: Vec<&tether_sde::MarketGroup> = match ids(params, "ids")? {
        Some(ids) => ids.iter().filter_map(|id| sde.market_group(*id)).collect(),
        None => sde.search_market_groups(param(params, "q").unwrap_or_default(), limit(params)),
    };
    Ok(Value::Array(
        found
            .iter()
            .map(|g| {
                let chain = sde.market_group_chain(g.id);
                let path = chain
                    .iter()
                    .take(3)
                    .rev()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ");
                json!({
                    "market_group_id": g.id,
                    "name": g.name,
                    "parent_id": g.parent_id,
                    "chain": chain.iter().map(|c| c.id).collect::<Vec<_>>(),
                    "path": path,
                })
            })
            .collect(),
    ))
}

/// `ids=` or `q=`: solar systems' names.
fn systems(sde: &Sde, params: &[(String, String)]) -> Result<Value, EsiError> {
    let found: Vec<(i64, &str)> = match ids(params, "ids")? {
        Some(ids) => ids
            .iter()
            .filter_map(|id| sde.system_name(*id).map(|n| (*id, n)))
            .collect(),
        None => sde.search_systems(param(params, "q").unwrap_or_default(), limit(params)),
    };
    Ok(Value::Array(
        found
            .iter()
            .map(|(id, name)| json!({ "system_id": id, "name": name }))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test code

    use super::get;

    fn ask(name: &str, params: &[(&str, &str)]) -> serde_json::Value {
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        serde_json::from_str(&get(name, &params).unwrap().unwrap()).unwrap()
    }

    #[test]
    fn apps_read_the_static_data() {
        let types = ask(
            "sde-types",
            &[
                ("name", "Veldspar"),
                ("name", "Rifter"),
                ("name", "No Such Thing"),
            ],
        );
        assert_eq!(types.as_array().unwrap().len(), 2);
        let named = |n: &str| {
            types
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == n)
                .unwrap()
                .clone()
        };
        let rifter = named("Rifter");
        assert_eq!(rifter["category_name"], "Ship");
        assert_eq!(rifter["packaged_volume"], 2500.0);
        assert!(rifter["market_group_chain"].as_array().unwrap().len() > 1);
        let veldspar = named("Veldspar")["type_id"].as_i64().unwrap().to_string();
        let materials = ask("sde-materials", &[("ids", &veldspar)]);
        assert_eq!(materials[0]["portion_size"], 100);
        assert!(!materials[0]["materials"].as_array().unwrap().is_empty());
        let search = ask(
            "sde-type-search",
            &[("q", "rifter"), ("exclude_category", "9")],
        );
        assert!(
            search
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["category_id"] != 9)
        );
        assert_eq!(search[0]["name"], "Rifter");
        let groups = ask("sde-market-groups", &[("q", "Veldspar")]);
        assert!(groups[0]["path"].as_str().unwrap().contains("->"));
        assert_eq!(
            ask("sde-systems", &[("ids", "30000142")])[0]["name"],
            "Jita"
        );
        // Not one of these: ESI's catalogue answers.
        assert!(get("universe-type", &[]).is_none());
        // Bad ids are refused, not guessed.
        assert!(
            get("sde-types", &[("ids".into(), "1,x".into())])
                .unwrap()
                .is_err()
        );
    }
}
