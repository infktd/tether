//! Plugin storage (N9): SQL in the plugin's own schema, through a pool that
//! connects as the plugin's own Postgres role.
//!
//! Postgres does the confining: the role can use and create objects only
//! in its schema (which Tether's role owns, so the plugin can't share it),
//! and has no rights on core, `public` or other plugins' schemas. The host
//! adds what a role can't express:
//!
//! - every call is one transaction, and before every statement the host
//!   sets the timeouts, memory and `search_path` itself, for the session:
//!   whatever a plugin `SET`s, `set_config`s or `ALTER ROLE`s (a role may
//!   change its own defaults) lasts until its next statement at most;
//! - statements are sent with the extended protocol, so one call is one
//!   statement, with parameters, never SQL built from data;
//! - caps on SQL size, parameters, statements per transaction, and the
//!   rows and bytes a result may carry (read row by row, stopping at the
//!   cap, so a huge result never sits in memory);
//! - error text that can't leak anything the plugin couldn't see anyway
//!   (Postgres messages about its own statements; connection trouble is
//!   logged, not passed on).

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgRow, PgTypeInfo};
use sqlx::{Arguments, Column, Encode, PgPool, Postgres, Row, Type, TypeInfo};

pub use crate::host::tether::plugin::storage::{DatabaseError, Error, Rows, Statement, Value};

/// SQL text per statement.
pub const MAX_SQL_BYTES: usize = 64 * 1024;
/// Parameters per statement.
pub const MAX_PARAMS: usize = 100;
/// All parameters of a call together (text, bytes, JSON).
pub const MAX_PARAM_BYTES: usize = 1024 * 1024;
/// Statements in one `transaction`.
pub const MAX_STATEMENTS: usize = 50;
/// Rows one `query` may return.
pub const MAX_ROWS: usize = 5_000;
/// Bytes one `query` may return: all text, bytes and JSON, plus
/// [`VALUE_COST`] per value.
pub const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const VALUE_COST: usize = 16;
/// Longest error message passed to the plugin.
const MAX_MESSAGE: usize = 1024;

/// Settings the host puts in place before every plugin statement. A role
/// can change its own defaults, so these are set each time rather than
/// trusted from the role.
pub const SESSION_SETTINGS: &[(&str, &str)] = &[
    ("statement_timeout", "5s"),
    ("lock_timeout", "2s"),
    ("idle_in_transaction_session_timeout", "10s"),
    ("work_mem", "16MB"),
    ("maintenance_work_mem", "64MB"),
];

/// The role's own defaults: the session settings (for its direct
/// connections, such as migrations), plus `temp_file_limit`, which only a
/// superuser can change, so the plugin can't lift it.
pub const ROLE_SETTINGS: &[(&str, &str)] = &[
    ("statement_timeout", "5s"),
    ("lock_timeout", "2s"),
    ("idle_in_transaction_session_timeout", "10s"),
    ("work_mem", "16MB"),
    ("maintenance_work_mem", "64MB"),
    ("temp_file_limit", "256MB"),
];

/// How long taking a connection from the plugin's pool may take.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// A plugin's storage: its pool (connected as its role) and schema.
#[derive(Clone)]
pub struct Storage {
    plugin: String,
    /// `"plugin_<id>"`, quoted, for `search_path`.
    search_path: String,
    pool: PgPool,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("plugin", &self.plugin)
            .finish_non_exhaustive()
    }
}

fn invalid(text: impl Into<String>) -> Error {
    Error::Invalid(text.into())
}

/// A `NULL` parameter whose type Postgres infers, as for a literal `NULL`.
struct UntypedNull;

impl Type<Postgres> for UntypedNull {
    fn type_info() -> PgTypeInfo {
        // Oid 0: "unspecified" in the extended protocol.
        PgTypeInfo::with_oid(Oid(0))
    }

    fn compatible(_: &PgTypeInfo) -> bool {
        true
    }
}

impl Encode<'_, Postgres> for UntypedNull {
    fn encode_by_ref(
        &self,
        _: &mut PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        Ok(sqlx::encode::IsNull::Yes)
    }
}

/// Checks a statement's size and its parameters, counting their bytes
/// against `budget`.
fn arguments(sql: &str, params: &[Value], budget: &mut usize) -> Result<PgArguments, Error> {
    if sql.len() > MAX_SQL_BYTES {
        return Err(invalid(format!(
            "a statement is longer than {MAX_SQL_BYTES} bytes"
        )));
    }
    if params.len() > MAX_PARAMS {
        return Err(invalid(format!(
            "a statement has more than {MAX_PARAMS} parameters"
        )));
    }
    let mut spend = |n: usize| {
        *budget = budget.saturating_add(n);
        if *budget > MAX_PARAM_BYTES {
            Err(invalid(format!(
                "the parameters are bigger than {MAX_PARAM_BYTES} bytes"
            )))
        } else {
            Ok(())
        }
    };
    let mut args = PgArguments::default();
    for (i, param) in params.iter().enumerate() {
        let n = i + 1;
        let added = match param {
            Value::Null => args.add(UntypedNull),
            Value::Boolean(b) => args.add(*b),
            Value::Integer(v) => args.add(*v),
            Value::Float(v) => args.add(*v),
            Value::Text(text) => {
                spend(text.len())?;
                args.add(text.clone())
            }
            Value::Bytes(bytes) => {
                spend(bytes.len())?;
                args.add(bytes.clone())
            }
            Value::Timestamp(text) => {
                let at = chrono::DateTime::parse_from_rfc3339(text)
                    .map_err(|_| invalid(format!("${n} isn't an RFC 3339 time")))?;
                args.add(at.with_timezone(&chrono::Utc))
            }
            Value::Json(text) => {
                spend(text.len())?;
                let json: serde_json::Value = serde_json::from_str(text)
                    .map_err(|_| invalid(format!("${n} isn't valid JSON")))?;
                args.add(sqlx::types::Json(json))
            }
        };
        added.map_err(|_| invalid(format!("${n} can't be sent")))?;
    }
    Ok(args)
}

fn column_value(row: &PgRow, i: usize) -> Result<Value, Error> {
    let column = &row.columns()[i];
    fn get<'r, T: sqlx::Decode<'r, Postgres> + Type<Postgres>>(
        row: &'r PgRow,
        i: usize,
    ) -> Result<Option<T>, Error> {
        row.try_get::<Option<T>, _>(i)
            .map_err(|_| invalid(format!("column {} couldn't be read", i + 1)))
    }
    let value = match column.type_info().name() {
        "BOOL" => get::<bool>(row, i)?.map(Value::Boolean),
        "INT2" => get::<i16>(row, i)?.map(|v| Value::Integer(v.into())),
        "INT4" => get::<i32>(row, i)?.map(|v| Value::Integer(v.into())),
        "INT8" => get::<i64>(row, i)?.map(Value::Integer),
        "FLOAT4" => get::<f32>(row, i)?.map(|v| Value::Float(v.into())),
        "FLOAT8" => get::<f64>(row, i)?.map(Value::Float),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => get::<String>(row, i)?.map(Value::Text),
        "BYTEA" => get::<Vec<u8>>(row, i)?.map(Value::Bytes),
        "TIMESTAMPTZ" => get::<chrono::DateTime<chrono::Utc>>(row, i)?
            .map(|at| Value::Timestamp(at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))),
        "TIMESTAMP" => get::<chrono::NaiveDateTime>(row, i)?.map(|at| {
            Value::Timestamp(
                at.and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            )
        }),
        "DATE" => get::<chrono::NaiveDate>(row, i)?.map(|d| Value::Text(d.to_string())),
        "JSON" | "JSONB" => get::<serde_json::Value>(row, i)?.map(|v| Value::Json(v.to_string())),
        "VOID" => None,
        other => {
            return Err(invalid(format!(
                "column {} ({}) is {other}, which storage can't return: cast it, e.g. ::text",
                i + 1,
                crate::host::printable(column.name(), 60)
            )));
        }
    };
    Ok(value.unwrap_or(Value::Null))
}

/// A plugin's statement, sent as it wrote it. Running the plugin's own SQL
/// is the point: Postgres confines it to the plugin's role and schema, and
/// data goes in as parameters.
fn plugin_sql(sql: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(sql.to_owned())
}

/// A column's size as it came off the wire (0 for NULL).
fn raw_len(row: &PgRow, i: usize) -> usize {
    use sqlx::ValueRef;
    row.try_get_raw(i)
        .ok()
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_bytes().ok().map(<[u8]>::len))
        .unwrap_or(0)
}

fn value_bytes(value: &Value) -> usize {
    VALUE_COST
        + match value {
            Value::Text(t) | Value::Timestamp(t) | Value::Json(t) => t.len(),
            Value::Bytes(b) => b.len(),
            _ => 0,
        }
}

impl Storage {
    /// `schema` is the plugin's schema name, made only of `[a-z0-9._-]`.
    pub fn new(plugin: &str, schema: &str, pool: PgPool) -> Self {
        Self {
            plugin: plugin.to_owned(),
            search_path: format!("\"{schema}\""),
            pool,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// What the plugin sees of a failure.
    fn error(&self, err: sqlx::Error) -> Error {
        match err {
            sqlx::Error::Database(db) => {
                let code = db.code().map(|c| c.into_owned()).unwrap_or_default();
                match code.as_str() {
                    // query_canceled (statement_timeout), lock_not_available
                    // (lock_timeout), idle_in_transaction_session_timeout.
                    "57014" | "55P03" | "25P03" => Error::Timeout,
                    _ => Error::Database(DatabaseError {
                        message: crate::host::printable(db.message(), MAX_MESSAGE),
                        code,
                    }),
                }
            }
            sqlx::Error::PoolTimedOut => Error::Timeout,
            other => {
                tracing::warn!(plugin = self.plugin, error = %other, "plugin storage failed");
                Error::Database(DatabaseError {
                    code: String::new(),
                    message: "the database couldn't be reached".to_owned(),
                })
            }
        }
    }

    async fn begin(&self) -> Result<sqlx::Transaction<'static, Postgres>, Error> {
        self.pool.begin().await.map_err(|e| self.error(e))
    }

    /// Puts Tether's settings in place before a statement, for the
    /// session, so they hold inside a transaction or out of it. The
    /// function is named with its schema: the plugin can create functions
    /// and change its role's `search_path`, so an unqualified name could
    /// resolve to a stand-in of its own.
    async fn reset(&self, tx: &mut sqlx::PgConnection) -> Result<(), Error> {
        let mut query = sqlx::QueryBuilder::<Postgres>::new("SELECT ");
        let settings = SESSION_SETTINGS
            .iter()
            .copied()
            .chain([("search_path", self.search_path.as_str())]);
        let mut separated = query.separated(", ");
        for (name, value) in settings {
            separated.push("pg_catalog.set_config(");
            separated.push_bind_unseparated(name);
            separated.push_unseparated(", ");
            separated.push_bind_unseparated(value);
            separated.push_unseparated(", false)");
        }
        query
            .build()
            .execute(tx)
            .await
            .map(|_| ())
            .map_err(|e| self.error(e))
    }

    pub async fn query(&self, sql: &str, params: &[Value]) -> Result<Rows, Error> {
        let args = arguments(sql, params, &mut 0)?;
        let mut tx = self.begin().await?;
        self.reset(&mut tx).await?;
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        {
            let mut stream = sqlx::query_with(plugin_sql(sql), args).fetch(&mut *tx);
            // `fetch` returns a boxed stream; its `poll_next` is callable
            // without importing the trait.
            while let Some(row) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                let row = row.map_err(|e| self.error(e))?;
                if rows.len() == MAX_ROWS {
                    return Err(Error::TooLarge);
                }
                if columns.is_empty() {
                    columns = row.columns().iter().map(|c| c.name().to_owned()).collect();
                    bytes += columns.iter().map(String::len).sum::<usize>();
                }
                // The driver has the row already; count its raw size before
                // decoding copies of anything in it.
                let raw: usize = (0..row.len()).map(|i| raw_len(&row, i) + VALUE_COST).sum();
                if bytes.saturating_add(raw) > MAX_RESULT_BYTES {
                    return Err(Error::TooLarge);
                }
                let mut values = Vec::with_capacity(row.len());
                for i in 0..row.len() {
                    let value = column_value(&row, i)?;
                    bytes += value_bytes(&value);
                    if bytes > MAX_RESULT_BYTES {
                        return Err(Error::TooLarge);
                    }
                    values.push(value);
                }
                rows.push(values);
            }
        }
        tx.commit().await.map_err(|e| self.error(e))?;
        Ok(Rows { columns, rows })
    }

    pub async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        let args = arguments(sql, params, &mut 0)?;
        let mut tx = self.begin().await?;
        self.reset(&mut tx).await?;
        let done = sqlx::query_with(plugin_sql(sql), args)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.error(e))?;
        tx.commit().await.map_err(|e| self.error(e))?;
        Ok(done.rows_affected())
    }

    pub async fn transaction(&self, statements: &[Statement]) -> Result<Vec<u64>, Error> {
        if statements.len() > MAX_STATEMENTS {
            return Err(invalid(format!(
                "a transaction has more than {MAX_STATEMENTS} statements"
            )));
        }
        // Check everything before touching the database.
        let mut budget = 0;
        let prepared = statements
            .iter()
            .map(|s| arguments(&s.sql, &s.params, &mut budget).map(|args| (s.sql.clone(), args)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut tx = self.begin().await?;
        let mut counts = Vec::with_capacity(prepared.len());
        for (sql, args) in prepared {
            self.reset(&mut tx).await?;
            let done = sqlx::query_with(plugin_sql(&sql), args)
                .execute(&mut *tx)
                .await
                .map_err(|e| self.error(e))?;
            counts.push(done.rows_affected());
        }
        tx.commit().await.map_err(|e| self.error(e))?;
        Ok(counts)
    }
}
