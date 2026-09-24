#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use serde_json::json;
use sqlx::PgPool;
use tether_cli::doctor::{self, Status};
use tether_cli::{Command, JobsCommand, TierArg, TiersCommand, UsersCommand, run};
use tether_db::accounts::{self, AccountId};
use tether_db::settings;
use tether_esi::Esi;
use tether_jobs::NewJob;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../tests/fixtures/esi/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

async fn mock_esi() -> (MockServer, Esi) {
    let server = MockServer::start().await;
    for (verb, route, file) in [
        ("POST", "/universe/names", "universe_names.json"),
        ("GET", "/status", "status.json"),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(fixture(file), "application/json"),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issuer": "test"})))
        .mount(&server)
        .await;
    let esi = Esi::new("tether tests", Some(&server.uri())).unwrap();
    (server, esi)
}

async fn cli(db: &PgPool, esi: &Esi, command: Command) -> anyhow::Result<String> {
    let mut out = Vec::new();
    run(command, db, esi, &mut out).await?;
    Ok(String::from_utf8(out).unwrap())
}

async fn account(db: &PgPool, id: i64, name: &str, owner: bool) -> AccountId {
    sign_in(db, id, name, None, owner).await
}

async fn sign_in(
    db: &PgPool,
    id: i64,
    name: &str,
    current: Option<AccountId>,
    owner: bool,
) -> AccountId {
    let login = accounts::Login {
        character_id: id,
        character_name: name,
        owner_hash: "h",
    };
    accounts::sign_in(db, login, current, owner)
        .await
        .unwrap()
        .outcome
        .account()
        .unwrap()
}

async fn audit_actors(db: &PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT action, actor_name FROM core.audit_log ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn users_lists_and_shows_accounts(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    let owner = account(&db, 196379789, "Chribba", true).await;
    sign_in(&db, 443630591, "The Mittani", Some(owner), false).await;
    account(&db, 1887431749, "gigX", false).await;

    let list = cli(&db, &esi, Command::Users { command: None })
        .await
        .unwrap();
    assert!(list.contains("Chribba (owner)"), "{list}");
    assert!(list.contains("gigX"));
    assert!(list.contains("2 account(s)"));

    // By character name (case-insensitive), finding the account via an alt.
    let show = cli(
        &db,
        &esi,
        Command::Users {
            command: Some(UsersCommand::Show {
                query: "the mittani".into(),
            }),
        },
    )
    .await
    .unwrap();
    assert!(
        show.contains(&format!("account {} (owner)", owner.0)),
        "{show}"
    );
    assert!(show.contains("Chribba  (main)"));
    assert!(show.contains("admin.audit"));

    let missing = cli(
        &db,
        &esi,
        Command::Users {
            command: Some(UsersCommand::Show {
                query: "nobody".into(),
            }),
        },
    )
    .await;
    assert!(
        missing
            .unwrap_err()
            .to_string()
            .contains("no account matches")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tiers_set_uses_esi_names_and_is_audited_as_cli(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;

    let out = cli(
        &db,
        &esi,
        Command::Tiers {
            command: Some(TiersCommand::Set {
                entity_id: 159826257,
                tier: TierArg::Member,
            }),
        },
    )
    .await
    .unwrap();
    assert!(
        out.contains("Otherworld Empire (alliance) is now member"),
        "{out}"
    );
    let listed = cli(&db, &esi, Command::Tiers { command: None })
        .await
        .unwrap();
    assert!(listed.contains("member  alliance"), "{listed}");

    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE kind = 'tiers.evaluate_all'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(queued, 1);

    // A character id is not an alliance or corporation.
    let not_org = cli(
        &db,
        &esi,
        Command::Tiers {
            command: Some(TiersCommand::Set {
                entity_id: 5,
                tier: TierArg::Member,
            }),
        },
    )
    .await;
    assert!(not_org.is_err());

    cli(
        &db,
        &esi,
        Command::Tiers {
            command: Some(TiersCommand::Remove {
                entity_id: 159826257,
            }),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        audit_actors(&db).await,
        [
            ("tier.rule.set".into(), Some("cli".into())),
            ("tier.rule.remove".into(), Some("cli".into()))
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn jobs_lists_and_retries_dead_jobs(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    let dead = tether_jobs::enqueue(&db, NewJob::new("tiers.refresh_account", json!({})))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE core.jobs SET state = 'dead', attempts = 5, last_error = 'ESI 503' WHERE id = $1",
    )
    .bind(dead.0)
    .execute(&db)
    .await
    .unwrap();
    tether_jobs::enqueue(&db, NewJob::new("other", json!({})))
        .await
        .unwrap();

    let out = cli(
        &db,
        &esi,
        Command::Jobs {
            command: None,
            state: None,
            limit: 20,
        },
    )
    .await
    .unwrap();
    assert!(
        out.starts_with("1 queued, 0 running, 0 succeeded, 1 dead"),
        "{out}"
    );
    assert!(out.contains("ESI 503"));

    cli(
        &db,
        &esi,
        Command::Jobs {
            command: Some(JobsCommand::Retry { job_id: dead.0 }),
            state: None,
            limit: 20,
        },
    )
    .await
    .unwrap();
    let out = cli(
        &db,
        &esi,
        Command::Jobs {
            command: None,
            state: None,
            limit: 20,
        },
    )
    .await
    .unwrap();
    assert!(
        out.starts_with("2 queued, 0 running, 0 succeeded, 0 dead"),
        "{out}"
    );

    let again = cli(
        &db,
        &esi,
        Command::Jobs {
            command: Some(JobsCommand::Retry { job_id: dead.0 }),
            state: None,
            limit: 20,
        },
    )
    .await;
    assert!(again.unwrap_err().to_string().contains("not dead"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn sync_queues_a_refresh_per_account(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    account(&db, 1, "A", false).await;
    account(&db, 2, "B", false).await;

    let out = cli(&db, &esi, Command::Sync).await.unwrap();

    assert!(out.contains("2 account(s)"));
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE kind = 'tiers.refresh_account'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(queued, 2);
    assert_eq!(
        audit_actors(&db).await,
        [("sync.trigger".into(), Some("cli".into()))]
    );
}

// ---- doctor ----------------------------------------------------------------

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_database_counts_migrations(db: PgPool) {
    // #[sqlx::test] applies migrations without the _sqlx_migrations rows the
    // app's own migrator writes, so run the migrator to get them.
    tether_db::migrate(&db).await.unwrap();
    let check = doctor::database(&db).await;
    assert_eq!(check.status, Status::Ok, "{check:?}");
}

#[tokio::test]
async fn doctor_dns() {
    let (check, ip) = doctor::dns("localhost").await.unwrap();
    assert_eq!(check.status, Status::Warn);
    assert!(ip.unwrap().is_loopback());
    let failed = doctor::dns("tether-doctor-test.invalid").await.unwrap_err();
    assert_eq!(failed.status, Status::Fail);
    assert!(
        failed
            .fix
            .unwrap()
            .contains("A (and optionally AAAA) record")
    );
}

#[tokio::test]
async fn doctor_ports() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let open = listener.local_addr().unwrap().port();
    let ip = "127.0.0.1".parse().unwrap();
    assert_eq!(doctor::port("port 443", ip, open).await.status, Status::Ok);
    let closed = doctor::port("port 80", ip, 1).await;
    assert_eq!(closed.status, Status::Fail);
    let fix = closed.fix.unwrap();
    assert!(fix.contains("Open TCP 1"));
    for layer in ["security lists", "network security groups", "iptables"] {
        assert!(fix.contains(layer), "fix should mention {layer}: {fix}");
    }
}

#[tokio::test]
async fn doctor_tls() {
    assert_eq!(
        doctor::tls("http://localhost:8080").await.status,
        Status::Warn
    );
    let unreachable = doctor::tls("https://127.0.0.1:1").await;
    assert_eq!(unreachable.status, Status::Fail);
    assert!(
        unreachable
            .fix
            .unwrap()
            .contains("docker compose logs caddy")
    );
}

#[tokio::test]
async fn doctor_esi() {
    let (_server, esi) = mock_esi().await;
    let ok = doctor::esi(&esi).await;
    assert_eq!(ok.status, Status::Ok);
    assert!(ok.detail.contains("19652 pilots online"));

    let down = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "down"})))
        .mount(&down)
        .await;
    let esi = Esi::new("tether tests", Some(&down.uri())).unwrap();
    assert_eq!(doctor::esi(&esi).await.status, Status::Fail);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_sso_reports_what_logins_have_proven(db: PgPool) {
    let (server, _) = mock_esi().await;
    let metadata = format!("{}/.well-known/oauth-authorization-server", server.uri());
    let site = "https://auth.example.com";

    let none = doctor::sso(&db, &metadata, site).await;
    assert_eq!(none.status, Status::Fail);

    settings::set(
        &db,
        settings::SSO_CLIENT_ID,
        json!("0123456789abcdef0123456789abcdef"),
    )
    .await
    .unwrap();
    let unproven = doctor::sso(&db, &metadata, site).await;
    assert_eq!(unproven.status, Status::Warn);
    assert!(
        unproven
            .fix
            .unwrap()
            .contains("https://auth.example.com/auth/callback")
    );

    settings::set(
        &db,
        settings::SSO_LAST_SUCCESS,
        json!({"at": "2026-09-24T10:00:00+00:00"}),
    )
    .await
    .unwrap();
    assert_eq!(doctor::sso(&db, &metadata, site).await.status, Status::Ok);

    settings::set(
        &db,
        settings::SSO_LAST_ERROR,
        json!({"at": "2026-09-24T11:00:00+00:00", "error": "invalid_client"}),
    )
    .await
    .unwrap();
    let failing = doctor::sso(&db, &metadata, site).await;
    assert_eq!(failing.status, Status::Fail);
    assert!(failing.detail.contains("invalid_client"));

    let unreachable = doctor::sso(&db, "http://127.0.0.1:1/metadata", site).await;
    assert_eq!(unreachable.status, Status::Fail);
    assert!(unreachable.fix.unwrap().contains("login.eveonline.com"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_setup_and_jobs(db: PgPool) {
    assert_eq!(doctor::setup(&db, "https://x").await.status, Status::Warn);
    account(&db, 1, "Owner", true).await;
    let no_rule = doctor::setup(&db, "https://x").await;
    assert!(no_rule.detail.contains("everyone is Guest"));

    assert_eq!(doctor::jobs(&db).await.status, Status::Ok);
    let id = tether_jobs::enqueue(&db, NewJob::new("x", json!({})))
        .await
        .unwrap();
    sqlx::query("UPDATE core.jobs SET state = 'dead' WHERE id = $1")
        .bind(id.0)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(doctor::jobs(&db).await.status, Status::Warn);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_prints_fixes_and_fails_overall(db: PgPool) {
    tether_db::migrate(&db).await.unwrap();
    let (server, esi) = mock_esi().await;
    let env = doctor::Env {
        db,
        esi,
        domain: "tether-doctor-test.invalid".into(),
        public_url: "https://tether-doctor-test.invalid".into(),
        sso_metadata_url: format!("{}/.well-known/oauth-authorization-server", server.uri()),
        http_port: 80,
        https_port: 443,
    };
    let mut out = Vec::new();

    let healthy = doctor::run(&env, &mut out).await.unwrap();

    let out = String::from_utf8(out).unwrap();
    assert!(!healthy);
    assert!(out.contains("[  ok] database"), "{out}");
    assert!(out.contains("[FAIL] dns"));
    assert!(out.contains("[skip] port 80: needs DNS"));
    assert!(out.contains("[skip] https: needs DNS"));
    assert!(out.contains("[skip] discord"));
    assert!(out.contains("fix: Create an A"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_skips_network_checks_for_localhost(db: PgPool) {
    tether_db::migrate(&db).await.unwrap();
    let (server, esi) = mock_esi().await;
    let env = doctor::Env {
        db,
        esi,
        domain: "localhost".into(),
        public_url: "https://localhost".into(),
        sso_metadata_url: format!("{}/.well-known/oauth-authorization-server", server.uri()),
        http_port: 80,
        https_port: 443,
    };
    let checks = doctor::checks(&env).await;
    for name in ["port 80", "port 443", "https", "public url"] {
        let check = checks.iter().find(|c| c.name == name).unwrap();
        assert_eq!(check.status, Status::Skip, "{check:?}");
    }
}
