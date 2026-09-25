#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use serde_json::json;
use sqlx::PgPool;
use tether_cli::doctor::{self, Status};
use tether_cli::{Command, JobsCommand, StatesCommand, UsersCommand, run};
use tether_core::Secret;
use tether_core::crypto::EncryptionKey;
use tether_db::accounts::{self, AccountId};
use tether_db::settings;
use tether_discord::{Discord, DiscordConfig, Endpoints};
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
async fn states_add_uses_esi_names_and_is_audited_as_cli(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    let add = |state: &str, entity_id| Command::States {
        command: Some(StatesCommand::Add {
            state: state.to_owned(),
            entity_id,
        }),
    };

    let out = cli(&db, &esi, add("member", 159826257)).await.unwrap();
    assert!(
        out.contains("Member now covers Otherworld Empire (alliance)"),
        "{out}"
    );
    let listed = cli(&db, &esi, Command::States { command: None })
        .await
        .unwrap();
    assert!(listed.contains("Member  (0 account(s))"), "{listed}");
    assert!(
        listed.contains("alliance        159826257  Otherworld Empire"),
        "{listed}"
    );

    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.jobs WHERE kind = 'states.evaluate_all'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(queued, 1);

    // Guest lists nobody, and unknown states are refused.
    assert!(cli(&db, &esi, add("Guest", 159826257)).await.is_err());
    assert!(cli(&db, &esi, add("Admirals", 159826257)).await.is_err());

    cli(
        &db,
        &esi,
        Command::States {
            command: Some(StatesCommand::Remove {
                state: "Member".to_owned(),
                entity_id: 159826257,
            }),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        audit_actors(&db).await,
        [
            ("state.add".into(), Some("cli".into())),
            ("state.remove".into(), Some("cli".into()))
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn jobs_lists_and_retries_dead_jobs(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    let dead = tether_jobs::enqueue(&db, NewJob::new("states.refresh_account", json!({})))
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
async fn sync_queues_one_affiliation_sync(db: PgPool) {
    let (_esi_server, esi) = mock_esi().await;
    account(&db, 1, "A", false).await;
    account(&db, 2, "B", false).await;

    let out = cli(&db, &esi, Command::Sync).await.unwrap();

    assert!(out.contains("Queued an affiliation sync"), "{out}");
    let queued: Vec<String> = sqlx::query_scalar("SELECT kind FROM core.jobs")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(queued, ["affiliation.sync"]);
    let audits = audit_actors(&db).await;
    assert_eq!(audits, [("sync.trigger".into(), Some("cli".into()))]);
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
        key: None,
        discord: unreachable_discord(),
        github_api_url: "http://127.0.0.1:9".into(),
    };
    let mut out = Vec::new();

    let healthy = doctor::run(&env, &mut out).await.unwrap();

    let out = String::from_utf8(out).unwrap();
    assert!(!healthy);
    assert!(out.contains("[  ok] database"), "{out}");
    assert!(out.contains("[FAIL] dns"));
    assert!(out.contains("[skip] port 80: needs DNS"));
    assert!(out.contains("[skip] https: needs DNS"));
    assert!(
        out.contains("[WARN] discord: ENCRYPTION_KEY isn't set"),
        "{out}"
    );
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
        key: None,
        discord: unreachable_discord(),
        github_api_url: "http://127.0.0.1:9".into(),
    };
    let checks = doctor::checks(&env).await;
    for name in ["port 80", "port 443", "https", "public url"] {
        let check = checks.iter().find(|c| c.name == name).unwrap();
        assert_eq!(check.status, Status::Skip, "{check:?}");
    }
}

fn local_net(host_port: &str) -> tether_net::Outbound {
    tether_net::Outbound::new(
        tether_net::Allowlist::production().with_local(host_port),
        "tether tests",
        std::time::Duration::from_secs(10),
    )
    .unwrap()
}

fn unreachable_discord() -> Discord {
    Discord::new(Endpoints::local("127.0.0.1:9"), local_net("127.0.0.1:9")).unwrap()
}

fn key(byte: &str) -> EncryptionKey {
    EncryptionKey::from_hex(&Secret::new(byte.repeat(32))).unwrap()
}

fn discord_fixture(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/../../tests/fixtures/discord/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

async fn discord_env(db: PgPool, key: Option<EncryptionKey>, server: &MockServer) -> doctor::Env {
    let (_, esi) = mock_esi().await;
    doctor::Env {
        db,
        esi,
        domain: "localhost".into(),
        public_url: "https://tether.test".into(),
        sso_metadata_url: "http://127.0.0.1:9/".into(),
        http_port: 80,
        https_port: 443,
        key,
        discord: Discord::new(
            Endpoints::local(server.address().to_string()),
            local_net(&server.address().to_string()),
        )
        .unwrap(),
        github_api_url: "http://127.0.0.1:9".into(),
    }
}

async fn save_discord(db: &PgPool, key: &EncryptionKey) {
    let config = DiscordConfig {
        application_id: 111_111_111_111_111_111,
        client_secret: Secret::new("client-secret".to_owned()),
        bot_token: Secret::new("bot-token".to_owned()),
        guild_id: 222_222_222_222_222_222,
    };
    let mut tx = db.begin().await.unwrap();
    tether_discord::store::save(&mut tx, key, &config)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_checks_the_discord_bot(db: PgPool) {
    let server = MockServer::start().await;
    for (route, fixture) in [
        ("/api/v10/users/@me", "bot_user"),
        ("/api/v10/guilds/222222222222222222", "guild"),
        ("/api/v10/guilds/222222222222222222/roles", "roles"),
        (
            "/api/v10/guilds/222222222222222222/members/111111111111111111",
            "bot_member",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(discord_fixture(fixture)))
            .mount(&server)
            .await;
    }

    // No key for this command: can't look.
    let check = doctor::discord(&discord_env(db.clone(), None, &server).await).await;
    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(check.fix.unwrap().contains("docker compose exec"));

    // Not set up yet.
    let check = doctor::discord(&discord_env(db.clone(), Some(key("01")), &server).await).await;
    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(
        check
            .fix
            .unwrap()
            .contains("https://tether.test/admin/discord")
    );

    save_discord(&db, &key("01")).await;
    let check = doctor::discord(&discord_env(db.clone(), Some(key("01")), &server).await).await;
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert_eq!(check.detail, "bot Tether is in New Miner's Union");

    // ENCRYPTION_KEY changed since the secrets were saved.
    let check = doctor::discord(&discord_env(db.clone(), Some(key("02")), &server).await).await;
    assert_eq!(check.status, Status::Fail, "{check:?}");
    assert!(check.fix.unwrap().contains("ENCRYPTION_KEY"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_says_how_to_fix_a_rejected_bot_token(db: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v10/users/@me"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"code": 0, "message": "401: Unauthorized"})),
        )
        .mount(&server)
        .await;
    save_discord(&db, &key("01")).await;
    let check = doctor::discord(&discord_env(db, Some(key("01")), &server).await).await;
    assert_eq!(check.status, Status::Fail, "{check:?}");
    assert!(check.fix.unwrap().contains("Reset the token"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn doctor_reports_update_checks(db: PgPool) {
    let github = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&github)
        .await;
    let on = doctor::updates(&db, &github.uri()).await;
    assert_eq!(on.status, Status::Ok, "{on:?}");
    assert!(on.detail.starts_with("on"));

    let unreachable = doctor::updates(&db, "http://127.0.0.1:9").await;
    assert_eq!(unreachable.status, Status::Warn, "{unreachable:?}");
    assert!(
        unreachable
            .fix
            .unwrap()
            .contains("switch update checks off")
    );

    settings::set(&db, "updates.enabled", json!(false))
        .await
        .unwrap();
    let off = doctor::updates(&db, "http://127.0.0.1:9").await;
    assert_eq!(off.status, Status::Ok);
    assert!(off.detail.starts_with("off"));
}

#[test]
fn doctor_proves_the_allow_list_holds() {
    let check = doctor::outbound();
    // CI and dev machines normally have no proxy set; either way the
    // self-test ran.
    assert!(
        matches!(check.status, Status::Ok | Status::Warn),
        "{check:?}"
    );
    if check.status == Status::Ok {
        assert!(check.detail.contains("esi.evetech.net"));
        assert!(check.detail.contains("Let's Encrypt"));
    }
}
