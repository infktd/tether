//! Market prices (aa-buybackprogram `ItemPrices`): Fuzzwork's aggregates
//! for a station, or Janice's pricer with the admin's API key, by the
//! Settings. One row a type for every program; a type first asked for is
//! fetched there and then (AA's `get_or_create_prices`), and every price
//! already kept is refreshed daily (`update_all_prices`), which also
//! refreshes ESI's averages (the "NPC" price) and purges calculations
//! nobody contracted.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::Value;
use tether_plugin_sdk::http::Request;
use tether_plugin_sdk::jobs::JobError;
use tether_plugin_sdk::storage::{self, Statement, Value as Db};
use tether_plugin_sdk::{esi, log};

use crate::pricing::Price;
use crate::{PUBLIC, Settings, float, int, retry, settings, when};

/// Types per Fuzzwork request (its URL) and per Janice request.
const FUZZWORK_CHUNK: usize = 200;
const JANICE_CHUNK: usize = 1000;
/// HTTP requests a refresh may make (the host allows 20 a run).
const REQUESTS_PER_RUN: usize = 18;

/// Kept prices of these types, fetching the ones never asked for when
/// `fetch` (a calculation; a page render never fetches).
pub fn get(type_ids: &[i64], fetch: bool) -> Result<HashMap<i64, Price>, String> {
    let mut ids = type_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut found = kept(&ids)?;
    let missing: Vec<i64> = ids
        .iter()
        .filter(|id| !found.contains_key(id))
        .copied()
        .collect();
    if fetch && !missing.is_empty() {
        let settings = settings().map_err(|e| format!("reading settings: {e:?}"))?;
        let fetched = fetch_prices(&settings, &missing, REQUESTS_PER_RUN)?;
        store(&fetched)?;
        for (id, buy, sell) in fetched {
            found.insert(
                id,
                Price {
                    buy,
                    sell,
                    age_hours: 0.0,
                },
            );
        }
    }
    Ok(found)
}

fn kept(ids: &[i64]) -> Result<HashMap<i64, Price>, String> {
    let rows = storage::query(
        "SELECT type_id, buy::float8, sell::float8, updated FROM item_prices \
         WHERE type_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint)",
        &[crate::json_ids(ids)],
    )
    .map_err(|e| format!("reading prices: {e:?}"))?;
    let now = Utc::now();
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            let age = when(r, 3).map_or(0.0, |t| (now - t).num_minutes() as f64 / 60.0);
            (
                int(r, 0),
                Price {
                    buy: float(r, 1),
                    sell: float(r, 2),
                    age_hours: age,
                },
            )
        })
        .collect())
}

fn store(prices: &[(i64, f64, f64)]) -> Result<(), String> {
    if prices.is_empty() {
        return Ok(());
    }
    let rows: Vec<Value> = prices
        .iter()
        .map(|(id, buy, sell)| serde_json::json!({ "type_id": id, "buy": buy, "sell": sell }))
        .collect();
    storage::execute(
        "INSERT INTO item_prices (type_id, buy, sell, updated) \
         SELECT type_id, buy, sell, now() FROM jsonb_to_recordset($1::jsonb) \
              AS x(type_id bigint, buy numeric, sell numeric) \
         ON CONFLICT (type_id) DO UPDATE SET buy = EXCLUDED.buy, sell = EXCLUDED.sell, \
              updated = now()",
        &[Db::json(Value::Array(rows).to_string())],
    )
    .map(|_| ())
    .map_err(|e| format!("storing prices: {e:?}"))
}

fn number(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

/// (type, buy, sell) for each asked type, at most `requests` requests;
/// a type the source doesn't answer for is 0, 0 (as AA).
fn fetch_prices(
    settings: &Settings,
    ids: &[i64],
    requests: usize,
) -> Result<Vec<(i64, f64, f64)>, String> {
    let janice = settings.price_method == "Janice";
    let chunk = if janice { JANICE_CHUNK } else { FUZZWORK_CHUNK };
    let mut out = Vec::new();
    for (n, part) in ids.chunks(chunk).enumerate() {
        if n >= requests {
            log::warn(format!(
                "prices: {} types left for the next run (request limit)",
                ids.len() - n * chunk
            ));
            break;
        }
        let answered = if janice {
            janice_prices(settings, part)?
        } else {
            fuzzwork_prices(settings, part)?
        };
        for id in part {
            let (buy, sell) = answered.get(id).copied().unwrap_or((0.0, 0.0));
            out.push((*id, buy, sell));
        }
    }
    Ok(out)
}

/// Fuzzwork's aggregates: `buy.percentile`/`sell.percentile` (the top 5%
/// average) or, with instant prices, `buy.max`/`sell.min`.
fn fuzzwork_prices(settings: &Settings, ids: &[i64]) -> Result<HashMap<i64, (f64, f64)>, String> {
    let url = format!(
        "https://market.fuzzwork.co.uk/aggregates/?station={}&types={}",
        settings.price_source_id,
        crate::id_list(ids)
    );
    let response = Request::get(url)
        .header("accept", "application/json")
        .send()
        .map_err(|e| format!("Fuzzwork: {e:?}"))?;
    if !response.is_success() {
        return Err(format!("Fuzzwork answered {}", response.status));
    }
    let body: Value =
        serde_json::from_slice(&response.body).map_err(|e| format!("Fuzzwork's answer: {e}"))?;
    let (buy_key, sell_key) = if settings.instant_prices {
        ("max", "min")
    } else {
        ("percentile", "percentile")
    };
    Ok(body
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(id, v)| {
                    Some((
                        id.parse::<i64>().ok()?,
                        (number(&v["buy"][buy_key]), number(&v["sell"][sell_key])),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Janice's pricer (market 2, Jita): its five-day medians, of the top 5%
/// average or, with instant prices, of the immediate prices.
fn janice_prices(settings: &Settings, ids: &[i64]) -> Result<HashMap<i64, (f64, f64)>, String> {
    let body = ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let response = Request::post(
        "https://janice.e-351.com/api/rest/v2/pricer?market=2",
        body.into_bytes(),
    )
    .header("content-type", "text/plain")
    .header("accept", "application/json")
    .secret("janice_api_key")
    .send()
    .map_err(|e| format!("Janice: {e:?}"))?;
    if !response.is_success() {
        return Err(format!(
            "Janice answered {} (is the API key set in Settings?)",
            response.status
        ));
    }
    let items: Value =
        serde_json::from_slice(&response.body).map_err(|e| format!("Janice's answer: {e}"))?;
    let group = if settings.instant_prices {
        "immediatePrices"
    } else {
        "top5AveragePrices"
    };
    Ok(items
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|item| {
                    Some((
                        item["itemType"]["eid"].as_i64()?,
                        (
                            number(&item[group]["buyPrice5DayMedian"]),
                            number(&item[group]["sellPrice5DayMedian"]),
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// ESI's average prices of these types (the "NPC" price of loot), read
/// from ESI first if none are kept yet.
pub fn npc(type_ids: &[i64]) -> Result<HashMap<i64, f64>, String> {
    let read = |ids: &[i64]| -> Result<HashMap<i64, f64>, String> {
        let rows = storage::query(
            "SELECT type_id, average::float8 FROM npc_prices \
             WHERE type_id IN (SELECT jsonb_array_elements_text($1::jsonb)::bigint)",
            &[crate::json_ids(ids)],
        )
        .map_err(|e| format!("reading NPC prices: {e:?}"))?;
        Ok(rows.rows.iter().map(|r| (int(r, 0), float(r, 1))).collect())
    };
    if type_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let found = read(type_ids)?;
    let any = storage::query("SELECT 1 FROM npc_prices LIMIT 1", &[])
        .map_err(|e| format!("reading NPC prices: {e:?}"))?;
    if any.rows.is_empty() {
        refresh_npc()?;
        return read(type_ids);
    }
    Ok(found)
}

/// Every type's ESI average price.
fn refresh_npc() -> Result<(), String> {
    let body = esi::get("markets-prices", PUBLIC, &[], None)
        .map_err(|e| format!("ESI's prices: {}", esi::describe(&e)))?
        .body;
    let prices: Value = serde_json::from_str(&body).map_err(|e| format!("ESI's prices: {e}"))?;
    let rows: Vec<Value> = prices
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(serde_json::json!({
                        "type_id": p["type_id"].as_i64()?,
                        "average": p["average_price"].as_f64()?,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    storage::execute(
        "INSERT INTO npc_prices (type_id, average, updated) \
         SELECT type_id, average, now() FROM jsonb_to_recordset($1::jsonb) \
              AS x(type_id bigint, average numeric) \
         ON CONFLICT (type_id) DO UPDATE SET average = EXCLUDED.average, updated = now()",
        &[Db::json(Value::Array(rows).to_string())],
    )
    .map(|_| ())
    .map_err(|e| format!("storing NPC prices: {e:?}"))
}

/// Daily: every kept price again, ESI's averages, and calculations
/// nobody contracted within the Settings' hours purged.
pub fn refresh_all() -> Result<(), JobError> {
    let settings = settings().map_err(|e| retry("reading settings", e))?;
    let ids: Vec<i64> = storage::query("SELECT type_id FROM item_prices ORDER BY updated", &[])
        .map_err(|e| retry("reading prices", e))?
        .rows
        .iter()
        .map(|r| int(r, 0))
        .collect();
    let fetched = fetch_prices(&settings, &ids, REQUESTS_PER_RUN).map_err(JobError::Retry)?;
    store(&fetched).map_err(JobError::Retry)?;
    if let Err(err) = refresh_npc() {
        log::warn(format!("NPC prices weren't refreshed: {err}"));
    }
    if settings.purge_hours > 0 {
        storage::transaction(&[
            Statement::new(
                "DELETE FROM trackings WHERE contract_id IS NULL \
                 AND created_at <= now() - make_interval(hours => $1::int)",
                vec![settings.purge_hours.into()],
            ),
            Statement::new(
                "DELETE FROM reverse_trackings WHERE contract_id IS NULL \
                 AND created_at <= now() - make_interval(hours => $1::int)",
                vec![settings.purge_hours.into()],
            ),
        ])
        .map_err(|e| retry("purging calculations", e))?;
    }
    storage::execute(
        "UPDATE settings SET prices_updated_at = now() WHERE id = 1",
        &[],
    )
    .map_err(|e| retry("noting the refresh", e))?;
    Ok(())
}
