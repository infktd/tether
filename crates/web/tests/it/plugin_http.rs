//! Plugin HTTP: only hosts an admin approved, redirects kept to them,
//! sizes capped, rate-limited, every request logged, and secrets added by
//! the host where declared, never seen by the plugin. Every approved host
//! is served by one wiremock, told apart by the `x-tether-test-host`
//! header the test build adds.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "nmu.http";
const API: &str = "api.example.com";
const OTHER: &str = "other.example.com";
const SECRET: &str = "s3cr3t-value-0123456789";

fn probe_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-storage"))
        .clone()
}

fn manifest(key: &Key, version: &str, hosts: &[&str], secret_header: &str) -> String {
    let hosts = hosts
        .iter()
        .map(|h| format!("\"{h}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[plugin]\nid = \"{ID}\"\nname = \"HTTP probe\"\nversion = \"{version}\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[capabilities]\nhttp = [{hosts}]\n\n\
         [capabilities.secrets.api_key]\nhost = \"{API}\"\nheader = \"{secret_header}\"\n\
         prefix = \"Key \"\n",
        key.public()
    )
}

fn package(manifest: &str) -> Vec<u8> {
    let component = probe_component();
    testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ])
}

/// The owner, the probe installed with both hosts approved, and a mock
/// standing in for them.
async fn setup(db: PgPool) -> (Harness, String, MockServer) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(7);
    let bytes = package(&manifest(&key, "1.0.0", &[API, OTHER], "X-Api-Key"));
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    let mock = MockServer::start().await;
    h.plugins.route_http_to(&mock.address().to_string());
    (h, owner, mock)
}

async fn http(h: &Harness, query: &[(&str, &str)]) -> String {
    let query = query
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    run_probe(h, ID, "http", query, false).await
}

async fn get(h: &Harness, url: &str) -> String {
    http(h, &[("url", url)]).await
}

type LogRow = (
    String,
    String,
    String,
    Option<i32>,
    String,
    Option<String>,
    i64,
);

async fn log(db: &PgPool) -> Vec<LogRow> {
    sqlx::query_as(
        "SELECT method, host, path, status, outcome, secret, bytes \
         FROM core.plugin_http_log ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

async fn received_for(mock: &MockServer, host: &str) -> Vec<wiremock::Request> {
    mock.received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| {
            r.headers
                .get("x-tether-test-host")
                .is_some_and(|v| v == host)
        })
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_approved_host_answers_and_every_request_is_logged(db: PgPool) {
    let (h, owner, mock) = setup(db).await;
    Mock::given(method("GET"))
        .and(path("/v1/thing"))
        .and(header("x-tether-test-host", API))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("etag", "\"abc\"")
                .insert_header("set-cookie", "session=1")
                .insert_header("x-internal", "nope")
                .set_body_string("{\"ok\":true}"),
        )
        .mount(&mock)
        .await;

    let out = http(
        &h,
        &[
            ("url", "https://api.example.com/v1/thing?q=private#frag"),
            ("h", "accept:application/json"),
        ],
    )
    .await;
    assert!(out.starts_with("ok status=200"), "{out}");
    assert!(out.contains("body={\"ok\":true}"), "{out}");
    assert!(out.contains("etag"), "{out}");
    assert!(
        !out.contains("set-cookie") && !out.contains("x-internal"),
        "{out}"
    );

    let seen = received_for(&mock, API).await;
    assert_eq!(seen.len(), 1);
    let agent = seen[0].headers.get("user-agent").unwrap().to_str().unwrap();
    assert_eq!(agent, "tether (app nmu.http)");
    assert_eq!(seen[0].url.query(), Some("q=private"));
    assert!(seen[0].headers.get("x-api-key").is_none());

    // Unknown paths still answer: the status is the plugin's to read.
    let out = get(&h, "https://other.example.com/missing").await;
    assert!(out.starts_with("ok status=404"), "{out}");

    // Logged with the path only: the query is never kept.
    let rows = log(&h.db).await;
    assert_eq!(
        rows[0],
        (
            "GET".to_owned(),
            API.to_owned(),
            "/v1/thing".to_owned(),
            Some(200),
            "ok".to_owned(),
            None,
            11
        )
    );
    assert_eq!(rows[1].1, OTHER);
    assert_eq!(rows[1].3, Some(404));

    // And shown on the app's admin page.
    let page = page(&h, "/admin/plugins/nmu.http", &owner).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("api.example.com/v1/thing"),
        "{}",
        page.body
    );
    assert!(!page.body.contains("q=private"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn other_hosts_schemes_and_ports_are_refused(db: PgPool) {
    let (h, _, mock) = setup(db).await;
    for url in [
        "https://evil.example/steal",
        "https://esi.evetech.net/latest/status/",
        "http://api.example.com/v1/thing",
        "https://api.example.com:8443/v1/thing",
        "https://127.0.0.1/",
        "https://user:pw@api.example.com/",
        "not a url",
    ] {
        let out = get(&h, url).await;
        assert!(out.starts_with("err Error::NotAllowed"), "{url}: {out}");
    }
    assert!(mock.received_requests().await.unwrap().is_empty());
    let rows = log(&h.db).await;
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().all(|r| r.4 == "not allowed"), "{rows:?}");
    assert_eq!(rows[0].1, "evil.example");
    assert_eq!(rows[6].1, "(not a URL)");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn redirects_are_followed_only_to_approved_hosts(db: PgPool) {
    let (h, _, mock) = setup(db).await;
    Mock::given(path("/hop"))
        .and(header("x-tether-test-host", API))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://other.example.com/landed"),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/landed"))
        .and(header("x-tether-test-host", OTHER))
        .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
        .mount(&mock)
        .await;
    Mock::given(path("/away"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://evil.example/steal"),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .mount(&mock)
        .await;

    let out = get(&h, "https://api.example.com/hop").await;
    assert!(
        out.starts_with("ok status=200") && out.ends_with("body=landed"),
        "{out}"
    );

    let out = get(&h, "https://api.example.com/away").await;
    assert!(
        out.starts_with("err Error::NotAllowed(\"redirected to evil.example"),
        "{out}"
    );
    let out = get(&h, "https://api.example.com/loop").await;
    assert!(out.contains("more than 3 redirects"), "{out}");

    let rows = log(&h.db).await;
    let outcomes: Vec<(&str, &str)> = rows.iter().map(|r| (r.1.as_str(), r.4.as_str())).collect();
    assert_eq!(
        &outcomes[..4],
        &[
            (API, "redirect"),
            (OTHER, "ok"),
            (API, "redirect"),
            ("evil.example", "redirect refused")
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn oversized_responses_and_bodies_are_refused(db: PgPool) {
    let (h, _, mock) = setup(db).await;
    Mock::given(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024 + 1]))
        .mount(&mock)
        .await;
    Mock::given(path("/fits"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024]))
        .mount(&mock)
        .await;
    assert_eq!(
        get(&h, "https://api.example.com/big").await,
        "err Error::TooLarge"
    );
    assert!(
        get(&h, "https://api.example.com/fits")
            .await
            .starts_with("ok status=200")
    );
    let body = "x".repeat(64 * 1024 + 1);
    let out = http(
        &h,
        &[
            ("url", "https://api.example.com/post"),
            ("method", "post"),
            ("body", &body),
        ],
    )
    .await;
    assert_eq!(out, "err Error::TooLarge");
    let rows = log(&h.db).await;
    assert_eq!(rows[0].4, "too large");
    assert_eq!(rows[0].3, Some(200));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn secrets_go_only_to_their_host_and_never_to_the_plugin(db: PgPool) {
    let (h, owner, mock) = setup(db).await;
    Mock::given(path("/keyed"))
        .and(header("x-tether-test-host", API))
        .and(header("x-api-key", format!("Key {SECRET}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_string("authorised"))
        .mount(&mock)
        .await;
    Mock::given(path("/keyed-hop"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", "https://other.example.com/elsewhere"),
        )
        .mount(&mock)
        .await;
    Mock::given(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_string("elsewhere"))
        .mount(&mock)
        .await;
    let keyed = [
        ("url", "https://api.example.com/keyed"),
        ("secret", "api_key"),
    ];

    // Not entered yet.
    let out = http(&h, &keyed).await;
    assert!(out.contains("hasn't been entered"), "{out}");

    // Only admins may enter it; it's never shown again.
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/secrets/api_key",
            &format!("value={SECRET}"),
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/secrets/nope",
            &format!("value={SECRET}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/secrets/api_key",
            &format!("value=%20{SECRET}%0A"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let admin = page(&h, "/admin/plugins/nmu.http", &owner).await.body;
    assert!(
        admin.contains("api_key") && !admin.contains(SECRET),
        "{admin}"
    );
    let sealed: Vec<u8> = sqlx::query_scalar(
        "SELECT sealed FROM core.secrets WHERE name = 'plugin-secret:nmu.http:api_key'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(!sealed.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
    let audit: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'plugin.secret_set'")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(audit, vec![serde_json::json!({ "secret": "api_key" })]);

    // The host adds it (the mock only answers with it), trimmed and with
    // its prefix; the plugin gets the answer, never the value.
    let out = http(&h, &keyed).await;
    assert!(
        out.starts_with("ok status=200") && out.ends_with("body=authorised"),
        "{out}"
    );
    assert!(!out.contains(SECRET));

    // Only to its own host, and not after a redirect elsewhere.
    let out = http(
        &h,
        &[
            ("url", "https://other.example.com/keyed"),
            ("secret", "api_key"),
        ],
    )
    .await;
    assert!(out.contains("only goes to api.example.com"), "{out}");
    let out = http(
        &h,
        &[
            ("url", "https://api.example.com/keyed-hop"),
            ("secret", "api_key"),
        ],
    )
    .await;
    assert!(
        out.starts_with("ok status=200") && out.ends_with("body=elsewhere"),
        "{out}"
    );
    let elsewhere = received_for(&mock, OTHER).await;
    assert_eq!(elsewhere.len(), 1);
    assert!(elsewhere[0].headers.get("x-api-key").is_none());
    // Not unless named, and never a secret that isn't the plugin's.
    get(&h, "https://api.example.com/plain").await;
    let plain: Vec<_> = received_for(&mock, API)
        .await
        .into_iter()
        .filter(|r| r.url.path() == "/plain")
        .collect();
    assert!(plain[0].headers.get("x-api-key").is_none());
    let out = http(
        &h,
        &[
            ("url", "https://api.example.com/keyed"),
            ("secret", "someone_elses"),
        ],
    )
    .await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");

    // The log names the secret, never its value.
    let logged: Vec<String> = sqlx::query_scalar(
        "SELECT concat_ws(' ', method, host, path, outcome, secret) FROM core.plugin_http_log",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(
        logged
            .iter()
            .any(|l| l == "GET api.example.com /keyed ok api_key"),
        "{logged:?}"
    );
    assert!(logged.iter().all(|l| !l.contains(SECRET)));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugins_cant_set_credentials_and_pages_only_get(db: PgPool) {
    let (h, _, mock) = setup(db).await;
    for header in [
        "authorization:Bearer mine",
        "Cookie:a=b",
        "proxy-authorization:x",
        "host:evil.example",
        "user-agent:me",
        "x-api-key:guess",
    ] {
        let out = http(&h, &[("url", "https://api.example.com/x"), ("h", header)]).await;
        assert!(out.starts_with("err Error::NotAllowed"), "{header}: {out}");
    }
    // A page render (a plain GET anyone can be linked into) can't POST.
    let query = vec![
        ("url".to_owned(), "https://api.example.com/x".to_owned()),
        ("method".to_owned(), "post".to_owned()),
    ];
    let out = run_probe(&h, ID, "http", query, true).await;
    assert!(out.contains("pages can only GET"), "{out}");
    assert!(mock.received_requests().await.unwrap().is_empty());
}

async fn repeat(h: &Harness, n: usize, as_page: bool) -> String {
    let query = vec![
        ("url".to_owned(), "https://api.example.com/r".to_owned()),
        ("n".to_owned(), n.to_string()),
    ];
    run_probe(h, ID, "http_repeat", query, as_page).await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn requests_are_capped_per_call_and_rate_limited(db: PgPool) {
    let (h, _, mock) = setup(db).await;
    Mock::given(path("/r"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock)
        .await;
    // 5 per page render, 20 per submit or job...
    assert_eq!(repeat(&h, 6, true).await, "ok=5 too_many=1");
    assert_eq!(repeat(&h, 25, false).await, "ok=20 too_many=5");
    // ...and 60 a minute per plugin.
    assert_eq!(repeat(&h, 20, false).await, "ok=20 too_many=0");
    assert_eq!(repeat(&h, 20, false).await, "ok=15 too_many=5");
    assert_eq!(mock.received_requests().await.unwrap().len(), 60);
    // What was sent is logged; attempts over the limit send nothing and
    // can't flood the log.
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_http_log")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(logged, 60);
    // Refused attempts count against the limit too.
    assert_eq!(get(&h, "not a url").await, "err Error::TooMany");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn hosts_a_new_version_adds_stay_refused_until_approved(db: PgPool) {
    let (h, owner, mock) = setup(db).await;
    Mock::given(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x"))
        .mount(&mock)
        .await;
    // A newer package (same key) swapped in without an approval: it adds
    // a host and moves the secret to another header.
    let key = Key::new(7);
    let bytes = package(&manifest(
        &key,
        "1.1.0",
        &[API, "new.example.com"],
        "X-Other-Key",
    ));
    use sha2::Digest;
    sqlx::query(
        "UPDATE core.plugins SET package = $2, signature = $3, package_sha256 = $4, \
         version = '1.1.0' WHERE id = $1",
    )
    .bind(ID)
    .bind(&bytes)
    .bind(key.sign(&bytes))
    .bind(sha2::Sha256::digest(&bytes).to_vec())
    .execute(&h.db)
    .await
    .unwrap();
    for action in ["disable", "enable"] {
        let res = send(
            &h.app,
            form(&format!("/admin/plugins/{ID}/{action}"), "", &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    assert!(h.plugins.get(ID).is_some());

    // The new host isn't approved; the old approved one still works; one
    // the new version dropped is gone.
    let out = get(&h, "https://new.example.com/x").await;
    assert!(
        out.contains("isn't one of this app's approved hosts"),
        "{out}"
    );
    assert!(get(&h, "https://api.example.com/x").await.starts_with("ok"));
    let out = get(&h, "https://other.example.com/x").await;
    assert!(out.starts_with("err Error::NotAllowed"), "{out}");
    // The secret's approved header no longer matches: refused.
    let out = http(
        &h,
        &[("url", "https://api.example.com/x"), ("secret", "api_key")],
    )
    .await;
    assert!(out.contains("approved secrets"), "{out}");
    let admin = page(&h, "/admin/plugins/nmu.http", &owner).await.body;
    assert!(admin.contains("new.example.com: not approved"), "{admin}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn packages_cant_declare_tethers_own_destinations(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(8);
    for host in [
        "esi.evetech.net",
        "cdn.discordapp.com",
        "raw.githubusercontent.com",
    ] {
        let manifest = format!(
            "[plugin]\nid = \"nmu.sneaky\"\nname = \"Sneaky\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
             [publisher]\nkey = \"{}\"\n\n[capabilities]\nhttp = [\"{host}\"]\n",
            key.public()
        );
        let bytes = package(&manifest);
        let res = upload(&h, &owner, &bytes, &key.sign(&bytes)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{host}");
        assert!(
            res.body.contains("Tether&#39;s own destinations"),
            "{}",
            res.body
        );
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn approving_again_drops_values_of_secrets_that_moved(db: PgPool) {
    use tether_db::plugin_http::{self, SecretSpec};
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(7);
    let bytes = package(&manifest(&key, "1.0.0", &[API, OTHER], "X-Api-Key"));
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/secrets/api_key",
            "value=abc",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let stored = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM core.secrets WHERE name = 'plugin-secret:nmu.http:api_key'",
        )
        .fetch_one(&h.db)
        .await
        .unwrap()
    };
    let account: i64 = sqlx::query_scalar("SELECT installed_by FROM core.plugins")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let approve = |header: &'static str| {
        let db = h.db.clone();
        async move {
            let mut tx = db.begin().await.unwrap();
            let spec = SecretSpec {
                name: "api_key".to_owned(),
                host: API.to_owned(),
                header: header.to_owned(),
                prefix: Some("Key ".to_owned()),
            };
            plugin_http::approve(
                &mut tx,
                ID,
                &[API.to_owned()],
                &[spec],
                tether_db::accounts::AccountId(account),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
    };
    // The same destination keeps the value; another header drops it.
    approve("X-Api-Key").await;
    assert_eq!(stored().await, 1);
    approve("X-New-Key").await;
    assert_eq!(stored().await, 0);
    // And uninstalling takes every value with it.
    send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/secrets/api_key",
            "value=abc",
            &owner,
        ),
    )
    .await;
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.http/uninstall",
            "confirmation=nmu.http",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.secrets WHERE name LIKE 'plugin-secret:%'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_log_is_pruned_by_age_and_count(db: PgPool) {
    for (plugin, age) in [("a.one", 100), ("a.one", 1), ("a.one", 0), ("a.two", 0)] {
        sqlx::query(
            "INSERT INTO core.plugin_http_log (plugin_id, method, host, path, outcome, at) \
             VALUES ($1, 'GET', 'x.example', '/', 'ok', now() - make_interval(days => $2))",
        )
        .bind(plugin)
        .bind(age)
        .execute(&db)
        .await
        .unwrap();
    }
    let pruned = tether_db::plugin_http::prune_log(&db, 90, 1).await.unwrap();
    assert_eq!(pruned, 2);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_http_log")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 2);
}
