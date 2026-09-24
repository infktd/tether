//! Runs whatever SQL a request carries through the storage API and shows
//! the outcome as text, so tests can probe what a plugin role may do.
//!
//! `?sql=...` (repeated for `transaction`) and `?p=<kind>:<value>` for
//! each parameter: `n:` null, `b:true`, `i:42`, `f:1.5`, `t:text`,
//! `ts:<rfc3339>`, `j:<json>`, `x:<hex bytes>`.

use tether_plugin_sdk::storage::{self, Statement, Value};
use tether_plugin_sdk::{Page, PageError, Plugin, Request};

struct Probe;

fn param(spec: &str) -> Value {
    let (kind, value) = spec.split_once(':').unwrap_or((spec, ""));
    match kind {
        "n" => Value::Null,
        "b" => Value::Boolean(value == "true"),
        "i" => Value::Integer(value.parse().unwrap_or_default()),
        "f" => Value::Float(value.parse().unwrap_or_default()),
        "ts" => Value::timestamp(value),
        "j" => Value::json(value),
        "x" => Value::Bytes(
            (0..value.len() / 2)
                .map(|i| u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap_or_default())
                .collect(),
        ),
        _ => Value::Text(value.to_owned()),
    }
}

fn shown(text: String) -> String {
    text.chars().take(1900).collect()
}

impl Plugin for Probe {
    fn render(request: Request) -> Result<Page, PageError> {
        let sql: Vec<&str> = request
            .query
            .iter()
            .filter(|(k, _)| k == "sql")
            .map(|(_, v)| v.as_str())
            .collect();
        let params: Vec<Value> = request
            .query
            .iter()
            .filter(|(k, _)| k == "p")
            .map(|(_, v)| param(v))
            .collect();
        let first = sql.first().copied().unwrap_or("");
        let outcome = match request.path.as_str() {
            "query" => match storage::query(first, &params) {
                Ok(rows) => format!("ok rows={} {:?}", rows.rows.len(), rows),
                Err(e) => format!("err {e:?}"),
            },
            "execute" => match storage::execute(first, &params) {
                Ok(n) => format!("ok changed={n}"),
                Err(e) => format!("err {e:?}"),
            },
            "transaction" => {
                let statements: Vec<Statement> = sql
                    .iter()
                    .map(|s| Statement::new(*s, params.clone()))
                    .collect();
                match storage::transaction(&statements) {
                    Ok(counts) => format!("ok changed={counts:?}"),
                    Err(e) => format!("err {e:?}"),
                }
            }
            _ => return Err(PageError::NotFound),
        };
        Ok(Page::new("Probe").text(shown(outcome)))
    }
}

tether_plugin_sdk::export!(Probe);
