#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Plugin storage (N9): a schema and role per plugin, confined by Postgres.
//! A probe plugin runs SQL through the host API; other tests connect as the
//! plugin's role directly, the way a plugin can't, to show the role itself
//! can do no more.

mod common;

use std::sync::OnceLock;

use axum::http::StatusCode;
use common::*;
use sqlx::{Connection, PgConnection, PgPool};
use tether_plugins::testing::{self, Key};
use tether_web::plugins::Status;

fn probe_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-storage"))
        .clone()
}

const NOTES: &str = "CREATE TABLE notes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    body text NOT NULL,
    n int4,
    f float8,
    ok boolean,
    at timestamptz,
    data jsonb,
    raw bytea,
    d date
);";

/// A signed probe package; `storage` says whether it asks for storage.
fn package(id: &str, key: &Key, storage: bool, migrations: &[(&str, &str)]) -> (Vec<u8>, String) {
    let manifest = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Probe\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nstorage = {storage}\n",
        key.public()
    );
    let component = probe_component();
    let mut files: Vec<(&str, &[u8])> = vec![
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ];
    for (name, sql) in migrations {
        files.push((name, sql.as_bytes()));
    }
    let bytes = testing::zip(&files);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

async fn install(h: &Harness, owner: &str, id: &str, migrations: &[(&str, &str)]) {
    let (bytes, signature) = package(id, &Key::new(1), true, migrations);
    let at = install_package(h, owner, &bytes, &signature).await;
    assert_eq!(at, format!("/admin/plugins/{id}"));
}

async fn uninstall(h: &Harness, owner: &str, id: &str) {
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{id}/uninstall"),
            &format!("confirmation={id}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

fn probe_query(sql: &[&str], params: &[&str]) -> Vec<(String, String)> {
    let mut query: Vec<(String, String)> = sql
        .iter()
        .map(|s| ("sql".to_owned(), (*s).to_owned()))
        .collect();
    query.extend(params.iter().map(|p| ("p".to_owned(), (*p).to_owned())));
    query
}

/// Runs the probe (through `submit`, where writes are allowed): `path` is
/// query, execute or transaction.
async fn probe(h: &Harness, id: &str, path: &str, sql: &[&str], params: &[&str]) -> String {
    run_probe(h, id, path, probe_query(sql, params), false).await
}

/// The same from a page render, which is read-only.
async fn probe_page(h: &Harness, id: &str, path: &str, sql: &[&str]) -> String {
    run_probe(h, id, path, probe_query(sql, &[]), true).await
}

async fn names(db: &PgPool, id: &str) -> tether_db::plugin_storage::Names {
    tether_db::plugin_storage::get(db, id)
        .await
        .unwrap()
        .unwrap()
}

/// A direct connection as the plugin's role.
async fn connect_as_plugin(h: &Harness, id: &str) -> PgConnection {
    let names = names(&h.db, id).await;
    let secret = format!("plugin.{id}.db_password");
    let sealed = tether_db::secrets::get(&h.db, &secret)
        .await
        .unwrap()
        .unwrap();
    let password = h
        .key
        .open(&sealed, &tether_db::secrets::context(&secret))
        .unwrap();
    let options =
        h.db.connect_options()
            .as_ref()
            .clone()
            .username(&names.role_name)
            .password(password.expose());
    PgConnection::connect_with(&options).await.unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plugin_keeps_data_in_its_own_schema(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(
        &h,
        &owner,
        "nmu.notes",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;
    assert_eq!(h.plugins.status("nmu.notes"), Status::Running);

    let inserted = probe(
        &h,
        "nmu.notes",
        "execute",
        &["INSERT INTO notes (body, n, f, ok, at, data, raw, d) \
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8::date)"],
        &[
            "t:o7",
            "i:42",
            "f:1.5",
            "b:true",
            "ts:2026-09-24T18:00:00Z",
            "j:{\"ore\": \"Bitumens\"}",
            "x:00ff",
            "t:2026-09-24",
        ],
    )
    .await;
    assert_eq!(inserted, "ok changed=1");
    // Untyped nulls work for any column type.
    let nulls = probe(
        &h,
        "nmu.notes",
        "execute",
        &["INSERT INTO notes (body, n, at, data) VALUES ($1, $2, $3, $4)"],
        &["t:empty", "n:", "n:", "n:"],
    )
    .await;
    assert_eq!(nulls, "ok changed=1");

    let rows = probe(
        &h,
        "nmu.notes",
        "query",
        &["SELECT body, n, f, ok, at, data, raw, d FROM notes ORDER BY id"],
        &[],
    )
    .await;
    for part in [
        "ok rows=2",
        "Text(\"o7\")",
        "Integer(42)",
        "Float(1.5)",
        "Boolean(true)",
        "Timestamp(\"2026-09-24T18:00:00Z\")",
        "Json(\"{\\\"ore\\\":\\\"Bitumens\\\"}\")",
        "Bytes([0, 255])",
        "Text(\"2026-09-24\")",
        "Null",
    ] {
        assert!(rows.contains(part), "{part}: {rows}");
    }

    // A transaction applies all or nothing.
    let failed = probe(
        &h,
        "nmu.notes",
        "transaction",
        &[
            "INSERT INTO notes (body) VALUES ('first')",
            "INSERT INTO notes (body) VALUES (NULL)",
        ],
        &[],
    )
    .await;
    assert!(failed.contains("23502"), "not-null violation: {failed}");
    let count = probe(
        &h,
        "nmu.notes",
        "query",
        &["SELECT count(*) FROM notes"],
        &[],
    )
    .await;
    assert!(count.contains("Integer(2)"), "{count}");

    // Types storage can't return must be cast.
    let numeric = probe(&h, "nmu.notes", "query", &["SELECT 1.5::numeric"], &[]).await;
    assert!(
        numeric.contains("Invalid") && numeric.contains("cast"),
        "{numeric}"
    );
    let cast = probe(
        &h,
        "nmu.notes",
        "query",
        &["SELECT 1.5::numeric::text"],
        &[],
    )
    .await;
    assert!(cast.contains("Text(\"1.5\")"), "{cast}");

    uninstall(&h, &owner, "nmu.notes").await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_plugin_role_can_reach_nothing_else(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(
        &h,
        &owner,
        "nmu.alpha",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;
    install(
        &h,
        &owner,
        "nmu.beta",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;

    let denied = [
        // Core tables, qualified or not.
        "SELECT * FROM core.accounts",
        "SELECT * FROM core.secrets",
        "SELECT * FROM accounts",
        // Another plugin's schema.
        "SELECT * FROM \"plugin_nmu.beta\".notes",
        // public, new schemas, temporary tables.
        "CREATE TABLE public.leak (x int)",
        "CREATE SCHEMA mine",
        "CREATE TEMP TABLE scratch (x int)",
        // Becoming someone else, or handing out its own schema.
        "SET ROLE tether",
        "SET SESSION AUTHORIZATION tether",
        // Advisory locks Tether itself takes.
        "SELECT pg_advisory_lock(1)",
        "SELECT pg_try_advisory_xact_lock(1)",
        // Server files.
        "SELECT pg_read_file('/etc/passwd')",
        "CREATE EXTENSION dblink",
    ];
    for sql in denied {
        let out = probe(&h, "nmu.alpha", "execute", &[sql], &[]).await;
        assert!(out.starts_with("err Error::Database"), "{sql}: {out}");
    }
    // Sharing with another plugin doesn't work either: alpha owns its
    // tables but not its schema, so its grants (a no-op on the schema, a
    // real one on the table) still leave beta outside.
    for sql in [
        "GRANT USAGE ON SCHEMA \"plugin_nmu.alpha\" TO PUBLIC",
        "GRANT SELECT ON notes TO PUBLIC",
    ] {
        probe(&h, "nmu.alpha", "execute", &[sql], &[]).await;
    }
    let peek = probe(
        &h,
        "nmu.beta",
        "query",
        &["SELECT * FROM \"plugin_nmu.alpha\".notes"],
        &[],
    )
    .await;
    assert!(peek.starts_with("err Error::Database"), "{peek}");

    // One statement per call.
    let two = probe(&h, "nmu.alpha", "execute", &["SELECT 1; SELECT 2"], &[]).await;
    assert!(two.starts_with("err Error::Database"), "{two}");

    // The role itself, connected directly, is just as confined.
    let names = names(&h.db, "nmu.alpha").await;
    let roles: (bool, bool, bool, bool, bool, i32) = sqlx::query_as(
        "SELECT rolsuper, rolcreatedb, rolcreaterole, rolbypassrls, rolinherit, rolconnlimit \
         FROM pg_roles WHERE rolname = $1",
    )
    .bind(&names.role_name)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(roles, (false, false, false, false, false, 4));
    let mut conn = connect_as_plugin(&h, "nmu.alpha").await;
    for sql in [
        "SELECT * FROM core.accounts",
        "SELECT * FROM \"plugin_nmu.beta\".notes",
        "SET temp_file_limit = '100GB'",
        "ALTER ROLE CURRENT_USER SET temp_file_limit = '100GB'",
        "ALTER ROLE CURRENT_USER CONNECTION LIMIT 100",
    ] {
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
            .execute(&mut conn)
            .await;
        assert!(result.is_err(), "{sql}");
    }
    // Its settings, from the role.
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(timeout, "5s");
    let path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(path, "\"plugin_nmu.alpha\"");
    conn.close().await.unwrap();

    uninstall(&h, &owner, "nmu.alpha").await;
    uninstall(&h, &owner, "nmu.beta").await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn limits_hold_whatever_the_plugin_sets(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, "nmu.slow", &[]).await;

    // Lifting the timeout lasts only until the next statement.
    let lifted = probe(
        &h,
        "nmu.slow",
        "transaction",
        &["SET statement_timeout = 0", "SELECT pg_sleep(6)"],
        &[],
    )
    .await;
    assert_eq!(lifted, "err Error::Timeout");
    let set = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT set_config('statement_timeout', '0', false)"],
        &[],
    )
    .await;
    assert!(set.starts_with("ok"), "{set}");
    let slept = probe(&h, "nmu.slow", "query", &["SELECT pg_sleep(6)"], &[]).await;
    assert_eq!(slept, "err Error::Timeout");
    // A role may change its own defaults; that doesn't help either.
    let altered = probe(
        &h,
        "nmu.slow",
        "execute",
        &["ALTER ROLE CURRENT_USER SET statement_timeout = 0"],
        &[],
    )
    .await;
    assert!(altered.starts_with("ok"), "{altered}");
    let slept = probe(&h, "nmu.slow", "query", &["SELECT pg_sleep(6)"], &[]).await;
    assert_eq!(slept, "err Error::Timeout");
    // Nor does a stand-in for set_config ahead of pg_catalog on its path,
    // with its role's defaults lifted too, on fresh connections.
    for sql in [
        "CREATE FUNCTION set_config(text, text, boolean) RETURNS text \
         LANGUAGE sql AS 'SELECT ''no-op''::text'",
        "CREATE FUNCTION pg_backend_pid() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        "ALTER ROLE CURRENT_USER SET search_path = \"plugin_nmu.slow\", pg_catalog",
        "ALTER ROLE CURRENT_USER SET statement_timeout = 0",
    ] {
        let out = probe(&h, "nmu.slow", "execute", &[sql], &[]).await;
        assert!(out.starts_with("ok"), "{sql}: {out}");
    }
    // Restarting it opens a new pool, whose sessions start from those
    // defaults.
    send(&h.app, form("/admin/plugins/nmu.slow/disable", "", &owner)).await;
    send(&h.app, form("/admin/plugins/nmu.slow/enable", "", &owner)).await;
    let slept = probe(&h, "nmu.slow", "query", &["SELECT pg_sleep(6)"], &[]).await;
    assert_eq!(slept, "err Error::Timeout");

    // Nor does pointing its search path elsewhere.
    probe(&h, "nmu.slow", "execute", &["SET search_path = core"], &[]).await;
    let path = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT current_setting('search_path')"],
        &[],
    )
    .await;
    assert!(path.contains("plugin_nmu.slow"), "{path}");

    // Results are capped in rows and bytes.
    let many = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT generate_series(1, 6000)"],
        &[],
    )
    .await;
    assert_eq!(many, "err Error::TooLarge");
    let wide = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT repeat('x', 1024 * 1024) FROM generate_series(1, 5)"],
        &[],
    )
    .await;
    assert_eq!(wide, "err Error::TooLarge");
    // One value bigger than the whole budget, refused by its raw size.
    let huge = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT repeat('x', 5 * 1024 * 1024)"],
        &[],
    )
    .await;
    assert_eq!(huge, "err Error::TooLarge");
    let fine = probe(
        &h,
        "nmu.slow",
        "query",
        &["SELECT generate_series(1, 100)"],
        &[],
    )
    .await;
    assert!(fine.starts_with("ok rows=100"), "{fine}");

    uninstall(&h, &owner, "nmu.slow").await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugins_without_storage_get_none(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (bytes, signature) = package("nmu.bare", &Key::new(1), false, &[]);
    install_package(&h, &owner, &bytes, &signature).await;
    let out = probe(&h, "nmu.bare", "query", &["SELECT 1"], &[]).await;
    assert_eq!(out, "err Error::NotApproved");
    assert!(
        tether_db::plugin_storage::get(&h.db, "nmu.bare")
            .await
            .unwrap()
            .is_none()
    );
    // Migrations without asking for storage make no sense.
    let (bytes, signature) = package(
        "nmu.confused",
        &Key::new(1),
        false,
        &[("migrations/0001_notes.sql", NOTES)],
    );
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("capabilities.storage"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn migrations_run_as_the_plugin_and_are_checked(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;

    // A migration can't reach core either: it runs as the plugin's role.
    install(
        &h,
        &owner,
        "nmu.sneaky",
        &[(
            "migrations/0001_steal.sql",
            "CREATE TABLE loot AS SELECT * FROM core.secrets;",
        )],
    )
    .await;
    match h.plugins.status("nmu.sneaky") {
        Status::Failed(why) => assert!(why.contains("0001_steal failed"), "{why}"),
        other => panic!("{other:?}"),
    }
    uninstall(&h, &owner, "nmu.sneaky").await;

    install(
        &h,
        &owner,
        "nmu.notes",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;
    let applied: Vec<(i32, String)> = sqlx::query_as(
        "SELECT version, name FROM core.plugin_migrations WHERE plugin_id = 'nmu.notes'",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(applied, [(1, "notes".to_owned())]);

    // An applied migration that changed is refused at the next load.
    sqlx::query("UPDATE core.plugin_migrations SET sha256 = '\\x00' WHERE plugin_id = 'nmu.notes'")
        .execute(&h.db)
        .await
        .unwrap();
    send(&h.app, form("/admin/plugins/nmu.notes/disable", "", &owner)).await;
    send(&h.app, form("/admin/plugins/nmu.notes/enable", "", &owner)).await;
    match h.plugins.status("nmu.notes") {
        Status::Failed(why) => assert!(why.contains("changed since it was applied"), "{why}"),
        other => panic!("{other:?}"),
    }
    uninstall(&h, &owner, "nmu.notes").await;
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn uninstalling_deletes_the_schema_and_role(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(
        &h,
        &owner,
        "nmu.notes",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;
    probe(
        &h,
        "nmu.notes",
        "execute",
        &["INSERT INTO notes (body) VALUES ('keep?')"],
        &[],
    )
    .await;
    let names = names(&h.db, "nmu.notes").await;
    // The page says what uninstalling does.
    let detail = page(&h, "/admin/plugins/nmu.notes", &owner).await.body;
    assert!(detail.contains("deletes its data"), "{detail}");

    uninstall(&h, &owner, "nmu.notes").await;
    let role: Option<String> =
        sqlx::query_scalar("SELECT rolname::text FROM pg_roles WHERE rolname = $1")
            .bind(&names.role_name)
            .fetch_optional(&h.db)
            .await
            .unwrap();
    assert!(role.is_none(), "the role is gone");
    let schema: Option<String> =
        sqlx::query_scalar("SELECT nspname::text FROM pg_namespace WHERE nspname = $1")
            .bind(&names.schema_name)
            .fetch_optional(&h.db)
            .await
            .unwrap();
    assert!(schema.is_none(), "the schema is gone");
    assert!(
        tether_db::secrets::get(&h.db, "plugin.nmu.notes.db_password")
            .await
            .unwrap()
            .is_none()
    );
    let audit: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'plugin.uninstalled'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(audit["data_deleted"], true);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn page_renders_are_read_only(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(
        &h,
        &owner,
        "nmu.notes",
        &[("migrations/0001_notes.sql", NOTES)],
    )
    .await;
    // A page (a GET anyone can be linked into) can read but not write,
    // whatever it tries.
    for sql in [
        "INSERT INTO notes (body) VALUES ('from a page')",
        "SET transaction_read_only = off",
    ] {
        let out = probe_page(&h, "nmu.notes", "execute", &[sql]).await;
        assert!(out.starts_with("err Error::Database"), "{sql}: {out}");
    }
    let out = probe_page(
        &h,
        "nmu.notes",
        "transaction",
        &["COMMIT", "INSERT INTO notes (body) VALUES ('after commit')"],
    )
    .await;
    assert!(out.starts_with("err"), "{out}");
    let read = probe_page(&h, "nmu.notes", "query", &["SELECT count(*) FROM notes"]).await;
    assert!(read.contains("Integer(0)"), "{read}");
    // The same statement from submit writes.
    let out = probe(
        &h,
        "nmu.notes",
        "execute",
        &["INSERT INTO notes (body) VALUES ('from submit')"],
        &[],
    )
    .await;
    assert_eq!(out, "ok changed=1");
    // And the connection goes back read-write for the next caller.
    let again = probe_page(&h, "nmu.notes", "query", &["SELECT count(*) FROM notes"]).await;
    assert!(again.contains("Integer(1)"), "{again}");
    let out = probe(
        &h,
        "nmu.notes",
        "execute",
        &["INSERT INTO notes (body) VALUES ('again')"],
        &[],
    )
    .await;
    assert_eq!(out, "ok changed=1");
    uninstall(&h, &owner, "nmu.notes").await;
}
