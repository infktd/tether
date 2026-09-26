//! Plugin upgrades and one-step rollback (F18, N14): the review shows what
//! a newer version asks for beyond the installed one, approving records
//! exactly that, and rolling back puts the earlier version (and, when the
//! upgrade changed its data, its data) back.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::Secret;
use tether_plugins::testing::{self, Key};
use tether_snapshots::{Config, Snapshots, Tools};
use tether_web::plugins::Status;

const HELLO: &str = "nmu.hello";
const NOTES: &str = "nmu.notes";

fn hello_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("hello-plugin"))
        .clone()
}

fn probe_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-storage"))
        .clone()
}

/// The hello plugin at `version`, asking for `http` hosts and holding
/// `permissions`.
fn hello(
    key: &Key,
    version: &str,
    http: &[&str],
    permissions: &[(&str, &str)],
) -> (Vec<u8>, String) {
    let hosts: Vec<String> = http.iter().map(|h| format!("\"{h}\"")).collect();
    let permissions: String = permissions
        .iter()
        .map(|(name, what)| format!("{name} = \"{what}\"\n"))
        .collect();
    let manifest = format!(
        "[plugin]\nid = \"{HELLO}\"\nname = \"Hello\"\nversion = \"{version}\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nhttp = [{}]\n\n[permissions]\n{permissions}",
        key.public(),
        hosts.join(", ")
    );
    let component = hello_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

/// The storage probe at `version`.
fn probe(
    key: &Key,
    version: &str,
    storage: bool,
    migrations: &[(&str, &str)],
) -> (Vec<u8>, String) {
    let manifest = format!(
        "[plugin]\nid = \"{NOTES}\"\nname = \"Notes\"\nversion = \"{version}\"\nhost_api = \"1\"\n\n\
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

const CREATE_NOTES: &str =
    "CREATE TABLE notes (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, body text NOT NULL);";
const ADD_TAG: &str = "ALTER TABLE notes ADD COLUMN tag text; UPDATE notes SET tag = 'old';";

fn running_version(h: &Harness, id: &str) -> String {
    h.plugins
        .running(id)
        .expect("the plugin is running")
        .manifest
        .plugin
        .version
        .clone()
}

async fn actions(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM core.audit_log WHERE action LIKE 'plugin.%' ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

async fn grants(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT permission FROM core.permission_grants WHERE permission LIKE 'plugin.%' ORDER BY 1",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

async fn declared(db: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT permission, description FROM core.plugin_permissions ORDER BY 1")
        .fetch_all(db)
        .await
        .unwrap()
}

async fn roll_back(h: &Harness, owner: &str, id: &str, confirmation: &str) -> Res {
    send(
        &h.app,
        form(
            &format!("/admin/plugins/{id}/rollback"),
            &format!("confirmation={confirmation}"),
            owner,
        ),
    )
    .await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_upgrade_shows_what_changes_and_rolls_back(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = hello(
        &key,
        "1.0.0",
        &["janice.e-351.com"],
        &[("view", "See the hello page"), ("report", "Read reports")],
    );
    install_package(&h, &owner, &bytes, &signature).await;
    for permission in ["plugin.nmu.hello.view", "plugin.nmu.hello.report"] {
        sqlx::query("INSERT INTO core.permission_grants (permission, state_id) VALUES ($1, $2)")
            .bind(permission)
            .bind(GUEST_STATE)
            .execute(&h.db)
            .await
            .unwrap();
    }
    // Nothing to roll back to yet.
    let shown = page(&h, "/admin/plugins/nmu.hello", &owner).await;
    assert!(!shown.body.contains("Roll back to"), "{}", shown.body);

    // 1.1.0 calls another host, adds a permission and drops one.
    let (bytes, signature) = hello(
        &key,
        "1.1.0",
        &["janice.e-351.com", "zkillboard.com"],
        &[
            ("view", "See the hello page, now in colour"),
            ("manage", "Change settings"),
        ],
    );
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review_at = res.location().to_owned();
    let review = page(&h, &review_at, &owner).await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    let what_changes = review
        .body
        .split("What changes")
        .nth(1)
        .unwrap()
        .split("What it asks for")
        .next()
        .unwrap();
    for part in [
        "zkillboard.com, which sees this server",
        "plugin.nmu.hello.manage",
        "plugin.nmu.hello.report",
        "every grant of it",
        "no new database migrations",
    ] {
        assert!(what_changes.contains(part), "{part}: {what_changes}");
    }
    assert!(
        !what_changes.contains("janice"),
        "unchanged: {what_changes}"
    );
    // Kept, with its grants, but described differently.
    assert!(what_changes.contains("Changed"), "{what_changes}");
    assert!(
        what_changes.contains("Now: See the hello page, now in colour"),
        "{what_changes}"
    );
    // The form carries what the review compared against: approving
    // against anything else is refused, and the upload stays.
    assert!(review.body.contains("name=\"reviewed\""), "{}", review.body);
    let stale = send(
        &h.app,
        form(&format!("{review_at}/approve"), "reviewed=none", &owner),
    )
    .await;
    assert_eq!(stale.status, StatusCode::BAD_REQUEST, "{}", stale.body);
    assert!(
        stale.body.contains("since this page was shown"),
        "{}",
        stale.body
    );
    assert_eq!(running_version(&h, HELLO), "1.0.0");
    assert!(review.body.contains("Approve and upgrade"));
    assert!(review.body.contains("Upgrade Hello"));

    let res = send(&h.app, form(&format!("{review_at}/approve"), "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/admin/plugins/nmu.hello");
    assert_eq!(h.plugins.status(HELLO), Status::Running);
    assert_eq!(running_version(&h, HELLO), "1.1.0");
    let approved = tether_db::plugin_http::approved(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(approved.hosts, ["janice.e-351.com", "zkillboard.com"]);
    // The kept permission keeps its grant; the dropped one's goes.
    assert_eq!(grants(&h.db).await, ["plugin.nmu.hello.view"]);
    assert_eq!(
        declared(&h.db).await,
        [
            (
                "plugin.nmu.hello.manage".to_owned(),
                "Change settings".to_owned()
            ),
            (
                "plugin.nmu.hello.view".to_owned(),
                "See the hello page, now in colour".to_owned()
            ),
        ]
    );
    let upgraded: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'plugin.upgraded'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(upgraded["from"], "1.0.0");
    assert_eq!(upgraded["to"], "1.1.0");
    assert_eq!(
        upgraded["grants_removed"][0]["permission"],
        "plugin.nmu.hello.report"
    );

    let shown = page(&h, "/admin/plugins/nmu.hello", &owner).await;
    assert!(shown.body.contains("Roll back to"), "{}", shown.body);
    assert!(!shown.body.contains("its data goes back"), "{}", shown.body);
    // What going back changes: zkillboard.com and manage go, report returns.
    let card = shown.body.split("Roll back to").nth(1).unwrap();
    for part in [
        "No longer",
        "zkillboard.com",
        "plugin.nmu.hello.manage",
        "plugin.nmu.hello.report",
    ] {
        assert!(card.contains(part), "{part}: {card}");
    }

    // The id must be typed.
    let res = roll_back(&h, &owner, HELLO, "nope").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(running_version(&h, HELLO), "1.1.0");

    let res = roll_back(&h, &owner, HELLO, HELLO).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(h.plugins.status(HELLO), Status::Running);
    assert_eq!(running_version(&h, HELLO), "1.0.0");
    let approved = tether_db::plugin_http::approved(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(approved.hosts, ["janice.e-351.com"]);
    // Back to 1.0.0's permissions: report is back, ungranted; manage goes.
    assert_eq!(grants(&h.db).await, ["plugin.nmu.hello.view"]);
    assert_eq!(
        declared(&h.db).await,
        [
            (
                "plugin.nmu.hello.report".to_owned(),
                "Read reports".to_owned()
            ),
            (
                "plugin.nmu.hello.view".to_owned(),
                "See the hello page".to_owned()
            ),
        ]
    );
    let installed = tether_db::plugins::get(&h.db, HELLO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(installed.version, "1.0.0");
    assert_eq!(installed.previous_version, None);
    // One step only.
    let shown = page(&h, "/admin/plugins/nmu.hello", &owner).await;
    assert!(!shown.body.contains("Roll back to"), "{}", shown.body);
    let res = roll_back(&h, &owner, HELLO, HELLO).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);

    let actions = actions(&h.db).await;
    assert!(
        actions.contains(&"plugin.upgraded".to_owned()),
        "{actions:?}"
    );
    assert!(
        actions.contains(&"plugin.rolled_back".to_owned()),
        "{actions:?}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_a_newer_compatible_version_upgrades(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = probe(
        &key,
        "1.0.0",
        true,
        &[("migrations/0001_notes.sql", CREATE_NOTES)],
    );
    install_package(&h, &owner, &bytes, &signature).await;

    let refused = [
        (
            probe(
                &key,
                "1.0.0",
                true,
                &[("migrations/0001_notes.sql", CREATE_NOTES)],
            ),
            "Only a newer version",
        ),
        (
            probe(
                &key,
                "0.9.0",
                true,
                &[("migrations/0001_notes.sql", CREATE_NOTES)],
            ),
            "Only a newer version",
        ),
        (
            probe(
                &key,
                "1.1.0",
                true,
                &[("migrations/0001_notes.sql", "CREATE TABLE notes (id int);")],
            ),
            "changed since it was applied",
        ),
        (probe(&key, "1.1.0", true, &[]), "this package doesn"),
        (
            probe(&key, "1.1.0", false, &[]),
            "ask for database storage, but",
        ),
        // Signed by someone else: the pin holds for upgrades too.
        (
            probe(
                &Key::new(2),
                "1.1.0",
                true,
                &[("migrations/0001_notes.sql", CREATE_NOTES)],
            ),
            "different key",
        ),
    ];
    for ((bytes, signature), why) in refused {
        let res = upload(&h, &owner, &bytes, &signature).await;
        assert!(res.status.is_client_error(), "{why}: {}", res.status);
        assert!(res.body.contains(why), "{why}: {}", res.body);
    }
    assert_eq!(running_version(&h, NOTES), "1.0.0");
    let rejected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'plugin.upload_rejected'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(rejected, 6);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn without_snapshots_a_data_change_cant_be_rolled_back(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let v1 = [("migrations/0001_notes.sql", CREATE_NOTES)];
    let v2 = [
        ("migrations/0001_notes.sql", CREATE_NOTES),
        ("migrations/0002_tag.sql", ADD_TAG),
    ];
    let (bytes, signature) = probe(&key, "1.0.0", true, &v1);
    install_package(&h, &owner, &bytes, &signature).await;
    let (bytes, signature) = probe(&key, "1.1.0", true, &v2);
    let res = upload(&h, &owner, &bytes, &signature).await;
    let review = page(&h, res.location(), &owner).await;
    assert!(
        review.body.contains("Snapshots are off here"),
        "{}",
        review.body
    );
    install_package(&h, &owner, &bytes, &signature).await;
    assert_eq!(running_version(&h, NOTES), "1.1.0");

    let shown = page(&h, "/admin/plugins/nmu.notes", &owner).await;
    assert!(
        shown.body.contains("snapshots are off here"),
        "{}",
        shown.body
    );
    let res = roll_back(&h, &owner, NOTES, NOTES).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(running_version(&h, NOTES), "1.1.0");
}

// ---- with snapshots ---------------------------------------------------------

fn database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        let env = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"))
            .expect("DATABASE_URL or a .env file");
        env.lines()
            .find_map(|l| l.strip_prefix("DATABASE_URL="))
            .expect("DATABASE_URL in .env")
            .trim()
            .to_owned()
    })
}

/// The URL of this test's own database.
async fn test_url(db: &PgPool) -> String {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(db)
        .await
        .unwrap();
    let base = database_url();
    let base = base.split('?').next().unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    format!("{server}/{name}")
}

/// Where the Postgres client tools are (`Some(None)` for PATH), or `None`
/// when this machine has none (never in CI). As in the CLI's rollback
/// tests, the dev database container's tools do.
async fn pg_bin(db: &PgPool) -> Option<Option<PathBuf>> {
    let major: i32 =
        sqlx::query_scalar("SELECT current_setting('server_version_num')::int / 10000")
            .fetch_one(db)
            .await
            .unwrap();
    let major = u32::try_from(major).unwrap();
    let mut candidates: Vec<Option<PathBuf>> = Vec::new();
    if let Ok(dir) = std::env::var("TETHER_TEST_PG_BIN") {
        candidates.push(Some(dir.into()));
    }
    candidates.push(None);
    for dir in [
        format!("/usr/lib/postgresql/{major}/bin"),
        format!("/opt/homebrew/opt/postgresql@{major}/bin"),
        format!("/usr/local/opt/postgresql@{major}/bin"),
    ] {
        candidates.push(Some(dir.into()));
    }
    for candidate in candidates {
        if Tools::new(candidate.clone()).check(major).await.is_ok() {
            return Some(candidate);
        }
    }
    if let Some(dir) = container_tools().await
        && Tools::new(Some(dir.clone())).check(major).await.is_ok()
    {
        return Some(Some(dir));
    }
    let why = format!("the Postgres {major} client tools aren't installed");
    if std::env::var_os("CI").is_some() {
        panic!("{why}");
    }
    #[allow(clippy::print_stderr)] // a skipped test says so
    {
        eprintln!("SKIPPED: {why}");
    }
    None
}

async fn container_tools() -> Option<PathBuf> {
    let container =
        std::env::var("TETHER_TEST_PG_CONTAINER").unwrap_or_else(|_| "tether-dev-db-1".to_owned());
    let running = tokio::process::Command::new("docker")
        .args(["inspect", "--format", "{{.State.Running}}", &container])
        .output()
        .await
        .ok()?;
    if String::from_utf8_lossy(&running.stdout).trim() != "true" {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("tether-pg-container-{container}"));
    std::fs::create_dir_all(&dir).unwrap();
    for tool in ["pg_dump", "pg_restore", "psql"] {
        // Written aside and renamed: tests run in parallel.
        let staged = dir.join(format!(
            ".{tool}.{}",
            &tether_core::new_token().unwrap().expose()[..8]
        ));
        std::fs::write(
            &staged,
            format!(
                "#!/bin/sh\nexec docker exec -i -e PGUSER -e PGPASSWORD -e PGDATABASE -e PGAPPNAME \
                 {container} {tool} \"$@\"\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staged, dir.join(tool)).unwrap();
    }
    Some(dir)
}

async fn notes(h: &Harness) -> String {
    run_probe(
        h,
        NOTES,
        "query",
        vec![(
            "sql".to_owned(),
            "SELECT body FROM notes ORDER BY id".to_owned(),
        )],
        false,
    )
    .await
}

async fn add_note(h: &Harness, body: &str) {
    let done = run_probe(
        h,
        NOTES,
        "execute",
        vec![
            (
                "sql".to_owned(),
                "INSERT INTO notes (body) VALUES ($1)".to_owned(),
            ),
            ("p".to_owned(), format!("t:{body}")),
        ],
        false,
    )
    .await;
    assert_eq!(done, "ok changed=1");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn rolling_back_puts_its_data_back_from_the_snapshot(db: PgPool) {
    let Some(bin) = pg_bin(&db).await else {
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "tether-upgrades-{}",
        &tether_core::new_token().unwrap().expose()[..8]
    ));
    let snapshots = Snapshots::new(Config {
        dir: dir.clone(),
        pg_bin_dir: bin,
        database_url: Secret::new(test_url(&db).await),
        key: test_key(),
    })
    .unwrap();
    let h = harness_with_snapshots(db, Arc::new(snapshots)).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = probe(
        &key,
        "1.0.0",
        true,
        &[("migrations/0001_notes.sql", CREATE_NOTES)],
    );
    install_package(&h, &owner, &bytes, &signature).await;
    add_note(&h, "before").await;

    let (bytes, signature) = probe(
        &key,
        "1.1.0",
        true,
        &[
            ("migrations/0001_notes.sql", CREATE_NOTES),
            ("migrations/0002_tag.sql", ADD_TAG),
        ],
    );
    let res = upload(&h, &owner, &bytes, &signature).await;
    let review = page(&h, res.location(), &owner).await;
    assert!(
        review.body.contains("A snapshot comes first"),
        "{}",
        review.body
    );
    install_package(&h, &owner, &bytes, &signature).await;
    assert_eq!(h.plugins.status(NOTES), Status::Running);
    assert_eq!(running_version(&h, NOTES), "1.1.0");
    add_note(&h, "after").await;
    assert!(notes(&h).await.contains("after"));

    let shown = page(&h, "/admin/plugins/nmu.notes", &owner).await;
    assert!(
        shown.body.contains("its data goes back to the snapshot"),
        "{}",
        shown.body
    );
    let res = roll_back(&h, &owner, NOTES, NOTES).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(h.plugins.status(NOTES), Status::Running);
    assert_eq!(running_version(&h, NOTES), "1.0.0");
    let rows = notes(&h).await;
    assert!(rows.contains("before") && !rows.contains("after"), "{rows}");
    let applied = tether_db::plugin_storage::applied(&h.db, NOTES)
        .await
        .unwrap();
    assert_eq!(applied.len(), 1);
    // The tag column went with the data.
    let tag = run_probe(
        &h,
        NOTES,
        "query",
        vec![("sql".to_owned(), "SELECT tag FROM notes".to_owned())],
        false,
    )
    .await;
    assert!(tag.contains("42703"), "undefined column: {tag}");
    let rolled_back: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM core.audit_log WHERE action = 'plugin.rolled_back'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(rolled_back["snapshot"]["name"].is_string(), "{rolled_back}");
    let _ = std::fs::remove_dir_all(&dir);
}
