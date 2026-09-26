//! Apps from GitHub (F15, F18): installing from a repository's releases,
//! the daily check for newer versions, and upgrading from it, against a
//! stand-in for GitHub's API and downloads.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HELLO: &str = "nmu.hello";

fn hello_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("hello-plugin"))
        .clone()
}

fn hello(key: &Key, id: &str, version: &str) -> (Vec<u8>, String) {
    let manifest = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Hello\"\nversion = \"{version}\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See the hello page\"\n",
        key.public()
    );
    let component = hello_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

/// A package and its signature.
type Signed = (Vec<u8>, String);

/// Publishes releases on the stand-in: each `(tag, asset name, package,
/// signature)`. Downloads redirect once to an asset host path, as
/// GitHub's do.
async fn publish(github: &MockServer, repo: &str, releases: &[(&str, &str, &Signed)]) {
    github.reset().await;
    let base = github.uri();
    let mut listed = Vec::new();
    for (n, (tag, name, (package, signature))) in releases.iter().enumerate() {
        let download = |file: &str| format!("{base}/{repo}/releases/download/{tag}/{file}");
        let sig_name = format!("{name}.minisig");
        listed.push(json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "html_url": format!("{base}/{repo}/releases/tag/{tag}"),
            "assets": [
                { "name": name, "size": package.len(), "browser_download_url": download(name) },
                { "name": sig_name, "size": signature.len(), "browser_download_url": download(&sig_name) },
            ],
        }));
        for (file, body) in [
            (name.to_string(), package.clone()),
            (sig_name, signature.clone().into_bytes()),
        ] {
            let asset = format!("/assets/{n}/{file}");
            Mock::given(method("GET"))
                .and(path(format!("/{repo}/releases/download/{tag}/{file}")))
                .respond_with(
                    ResponseTemplate::new(302).insert_header("location", format!("{base}{asset}")),
                )
                .mount(github)
                .await;
            Mock::given(method("GET"))
                .and(path(asset))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .mount(github)
                .await;
        }
    }
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repo}/releases")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(listed)))
        .mount(github)
        .await;
}

async fn install_from(h: &Harness, owner: &str, repo: &str, app: &str) -> Res {
    send(
        &h.app,
        form(
            "/admin/plugin-github",
            &format!("repo={}&app={app}", urlencode(repo)),
            owner,
        ),
    )
    .await
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn approve(h: &Harness, owner: &str, review: &str) {
    let res = send(&h.app, form(&format!("{review}/approve"), "", owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_app_installs_from_github_and_upgrades_from_it(db: PgPool) {
    let github = MockServer::start().await;
    let h = harness_with_github(db, &github).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let v1 = hello(&key, HELLO, "1.0.0");
    publish(&github, "nmu/apps", &[("v1", "nmu.hello-1.0.0.zip", &v1)]).await;

    let res = install_from(&h, &owner, "https://github.com/nmu/apps", "").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review = res.location().to_owned();
    assert!(review.starts_with("/admin/plugin-uploads/"), "{review}");
    approve(&h, &owner, &review).await;
    let installed = tether_db::plugins::get(&h.db, HELLO)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(installed.version, "1.0.0");
    let status = tether_db::plugin_sources::status(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(status.source.as_deref(), Some("nmu/apps"));
    let uploaded: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'plugin.uploaded'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(uploaded["source"], "nmu/apps");

    // Nothing newer yet.
    let github_client = h.plugins.github().unwrap().clone();
    tether_web::plugin_github::check_all(&h.db, &github_client)
        .await
        .unwrap();
    let list = page(&h, "/admin/plugins", &owner).await;
    assert!(!list.body.contains("available"), "{}", list.body);

    // 1.1.0 is published: the check finds it, and the upgrade is reviewed.
    let v2 = hello(&key, HELLO, "1.1.0");
    publish(
        &github,
        "nmu/apps",
        &[
            ("v2", "nmu.hello-1.1.0.zip", &v2),
            ("v1", "nmu.hello-1.0.0.zip", &v1),
        ],
    )
    .await;
    tether_web::plugin_github::check_all(&h.db, &github_client)
        .await
        .unwrap();
    let list = page(&h, "/admin/plugins", &owner).await;
    assert!(
        list.body.contains("1.1.0</span> available"),
        "{}",
        list.body
    );
    let shown = page(&h, "/admin/plugins/nmu.hello", &owner).await;
    assert!(shown.body.contains("Review version"), "{}", shown.body);
    assert!(shown.body.contains("release notes"), "{}", shown.body);

    let res = send(&h.app, form("/admin/plugins/nmu.hello/update", "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review = res.location().to_owned();
    let reviewed = page(&h, &review, &owner).await;
    assert!(
        reviewed.body.contains("Fetched from GitHub:"),
        "{}",
        reviewed.body
    );
    assert!(
        reviewed.body.contains("Approve and upgrade"),
        "{}",
        reviewed.body
    );
    approve(&h, &owner, &review).await;
    assert_eq!(
        h.plugins.running(HELLO).unwrap().manifest.plugin.version,
        "1.1.0"
    );
    // Up to date now: nothing to fetch.
    let res = send(&h.app, form("/admin/plugins/nmu.hello/update", "", &owner)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(
        res.body.contains("newest nmu/apps publishes is 1.1.0"),
        "{}",
        res.body
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn what_github_fetches_is_checked(db: PgPool) {
    let github = MockServer::start().await;
    let h = harness_with_github(db, &github).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);

    // Not a repository.
    let res = install_from(&h, &owner, "https://gitlab.com/nmu/apps", "").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    // Several apps: which one must be said.
    let hello_pkg = hello(&key, HELLO, "1.0.0");
    let other = hello(&key, "nmu.other", "2.0.0");
    publish(
        &github,
        "nmu/apps",
        &[
            ("v1", "nmu.hello-1.0.0.zip", &hello_pkg),
            ("v2", "nmu.other-2.0.0.zip", &other),
        ],
    )
    .await;
    let res = install_from(&h, &owner, "nmu/apps", "").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("publishes several apps"), "{}", res.body);
    let res = install_from(&h, &owner, "nmu/apps", "nmu.other").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // An asset named for one app holding another is refused.
    let disguised = hello(&key, "nmu.other", "3.0.0");
    publish(
        &github,
        "nmu/apps",
        &[("v3", "nmu.hello-3.0.0.zip", &disguised)],
    )
    .await;
    let res = install_from(&h, &owner, "nmu/apps", "nmu.hello").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(
        res.body
            .contains("holds app nmu.other version 3.0.0 instead"),
        "{}",
        res.body
    );

    // Nor one named for another version.
    let mislabelled = hello(&key, HELLO, "1.0.0");
    publish(
        &github,
        "nmu/apps",
        &[("v9", "nmu.hello-9.9.9.zip", &mislabelled)],
    )
    .await;
    let res = install_from(&h, &owner, "nmu/apps", "nmu.hello").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("version 1.0.0 instead"), "{}", res.body);

    // A signature by someone else doesn't check out.
    let (package, _) = hello(&key, HELLO, "1.0.0");
    let forged = (package.clone(), Key::new(2).sign(&package));
    publish(
        &github,
        "nmu/apps",
        &[("v1", "nmu.hello-1.0.0.zip", &forged)],
    )
    .await;
    let res = install_from(&h, &owner, "nmu/apps", "").await;
    assert!(res.status.is_client_error(), "{}", res.status);

    // No repository.
    github.reset().await;
    let res = install_from(&h, &owner, "nmu/gone", "").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(
        res.body.contains("no public repository nmu/gone"),
        "{}",
        res.body
    );

    let rejected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'plugin.upload_rejected'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(rejected >= 5, "{rejected}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_repository_can_be_named_later_and_checks_can_be_off(db: PgPool) {
    let github = MockServer::start().await;
    let h = harness_with_github(db, &github).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = hello(&key, HELLO, "1.0.0");
    install_package(&h, &owner, &bytes, &signature).await;
    let status = tether_db::plugin_sources::status(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(status.source, None);

    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.hello/source",
            "source=not+a+repo",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.hello/source",
            "source=https%3A%2F%2Fgithub.com%2Fnmu%2Fapps",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let status = tether_db::plugin_sources::status(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(status.source.as_deref(), Some("nmu/apps"));
    // A check is queued for it.
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'plugins.update_check' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(queued, 1);

    // With update checks off, the check contacts nobody.
    tether_db::settings::set(&h.db, "updates.enabled", false.into())
        .await
        .unwrap();
    let github_client = h.plugins.github().unwrap().clone();
    tether_web::plugin_github::check_all(&h.db, &github_client)
        .await
        .unwrap();
    assert!(github.received_requests().await.unwrap().is_empty());
    let status = tether_db::plugin_sources::status(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(status.checked_at, None);

    // Cleared: no longer looked for.
    let res = send(
        &h.app,
        form("/admin/plugins/nmu.hello/source", "source=", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let status = tether_db::plugin_sources::status(&h.db, HELLO)
        .await
        .unwrap();
    assert_eq!(status.source, None);
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM core.audit_log WHERE action = 'plugin.source_set'")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(actions.len(), 2);
}
