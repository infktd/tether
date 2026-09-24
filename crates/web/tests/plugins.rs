#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! The plugin lifecycle: upload, review, approve (the plugin runs with no
//! restart), disable, enable, uninstall, re-pin; what each refuses, and
//! that each is audited.

mod common;

use std::sync::OnceLock;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_plugins::host::Request as PageRequest;
use tether_plugins::testing::{self, Key};
use tether_web::plugins::{Plugins, Status};

const ID: &str = "nmu.hello";

/// The example plugin, built once per test run.
fn hello_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("hello-plugin"))
        .clone()
}

fn manifest(id: &str, key: &Key, extra: &str) -> String {
    format!(
        "[plugin]\nid = \"{id}\"\nname = \"Hello\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\
         description = \"Says hello\"\n\n[publisher]\nkey = \"{}\"\n\n\
         [capabilities]\nhttp = [\"janice.e-351.com\"]\n\n\
         [permissions]\nview = \"See the hello page\"\n{extra}",
        key.public()
    )
}

/// A signed package of the hello plugin.
fn package(id: &str, key: &Key, extra: &str) -> (Vec<u8>, String) {
    let manifest = manifest(id, key, extra);
    let component = hello_component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

async fn actions(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT action FROM core.audit_log WHERE action LIKE 'plugin.%' ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

/// Uploads and approves the hello plugin.
async fn install(h: &Harness, token: &str, key: &Key) {
    let (bytes, signature) = package(ID, key, "");
    let res = upload(h, token, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review = res.location().to_owned();
    let res = send(&h.app, form(&format!("{review}/approve"), "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/admin/plugins/{ID}"));
}

async fn renders(plugins: &Plugins) -> String {
    let plugin = plugins.get(ID).expect("the plugin is running");
    let rendered = plugins
        .host()
        .render(
            &plugin,
            PageRequest {
                path: String::new(),
                query: Vec::new(),
            },
            &Default::default(),
        )
        .await
        .unwrap();
    rendered.page.title
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_admin_needs_admin_plugins(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;

    for uri in [
        "/admin/plugins",
        "/admin/plugin-uploads/1",
        "/admin/plugins/nmu.hello",
        "/admin/plugin-keys/nmu.hello",
    ] {
        assert_eq!(send(&h.app, get(uri, &[])).await.location(), "/login");
        assert_eq!(
            page(&h, uri, &pilot).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    for uri in [
        "/admin/plugin-uploads/1/approve",
        "/admin/plugin-uploads/1/discard",
        "/admin/plugins/nmu.hello/enable",
        "/admin/plugins/nmu.hello/disable",
        "/admin/plugins/nmu.hello/uninstall",
        "/admin/plugin-keys/nmu.hello",
    ] {
        assert_eq!(
            send(&h.app, form(uri, "confirmation=nmu.hello", &pilot))
                .await
                .status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    // The permission is checked before the body is read: a body far over
    // the upload limit gets 403, not 413.
    let huge = vec![0u8; tether_web::pages::plugins::UPLOAD_BODY_LIMIT + 1];
    let res = send(&h.app, upload_request(&pilot, huge.clone())).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(&h.app, upload_request(&owner, huge)).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", res.body);

    let list = page(&h, "/admin/plugins", &owner).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(
        list.body.contains(r#"href="/admin/plugins""#),
        "sidebar link"
    );
    assert!(list.body.contains("No plugins yet"));
    assert_eq!(
        page(&h, "/admin/plugins/Not.An.Id", &owner).await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_approved_plugin_runs_without_a_restart(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = package(ID, &key, "");

    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review_uri = res.location().to_owned();
    assert!(review_uri.starts_with("/admin/plugin-uploads/"));
    // Nothing is installed or running before approval.
    assert_eq!(h.plugins.status(ID), Status::Stopped);
    let list = page(&h, "/admin/plugins", &owner).await.body;
    assert!(
        list.contains("Waiting for approval") && list.contains(ID),
        "{list}"
    );

    let review = page(&h, &review_uri, &owner).await;
    assert_eq!(review.status, StatusCode::OK);
    for text in [
        "Hello",
        "New publisher key",
        "janice.e-351.com",
        "plugin.nmu.hello.view",
        "See the hello page",
        &key.public(),
    ] {
        assert!(review.body.contains(text), "{text}: {}", review.body);
    }

    let res = send(&h.app, form(&format!("{review_uri}/approve"), "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/admin/plugins/nmu.hello");
    assert_eq!(h.plugins.status(ID), Status::Running);
    assert_eq!(renders(&h.plugins).await, "Hello");

    let detail = page(&h, "/admin/plugins/nmu.hello", &owner).await;
    assert_eq!(detail.status, StatusCode::OK);
    assert!(detail.body.contains("Running"), "{}", detail.body);
    // The upload is used up.
    assert_eq!(
        page(&h, &review_uri, &owner).await.status,
        StatusCode::NOT_FOUND
    );
    let res = send(&h.app, form(&format!("{review_uri}/approve"), "", &owner)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    assert_eq!(
        actions(&h.db).await,
        ["plugin.uploaded", "plugin.key_pinned", "plugin.installed"]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn disable_enable_and_uninstall(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    install(&h, &owner, &key).await;

    let res = send(&h.app, form("/admin/plugins/nmu.hello/disable", "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(h.plugins.status(ID), Status::Stopped);
    assert!(h.plugins.get(ID).is_none());
    assert!(
        page(&h, "/admin/plugins", &owner)
            .await
            .body
            .contains("Disabled")
    );
    // Again: nothing changes, nothing more audited.
    send(&h.app, form("/admin/plugins/nmu.hello/disable", "", &owner)).await;

    send(&h.app, form("/admin/plugins/nmu.hello/enable", "", &owner)).await;
    assert_eq!(h.plugins.status(ID), Status::Running);

    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.hello/uninstall",
            "confirmation=nmu.hell",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(h.plugins.status(ID), Status::Running);

    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.hello/uninstall",
            "confirmation=nmu.hello",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/admin/plugins");
    assert_eq!(h.plugins.status(ID), Status::Stopped);
    assert_eq!(
        page(&h, "/admin/plugins/nmu.hello", &owner).await.status,
        StatusCode::NOT_FOUND
    );
    for uri in ["enable", "disable"] {
        let res = send(
            &h.app,
            form(&format!("/admin/plugins/nmu.hello/{uri}"), "", &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{uri}");
    }
    // The pin stays: another publisher still can't take the id.
    let (bytes, signature) = package(ID, &Key::new(2), "");
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(res.body.contains("re-pin"), "{}", res.body);

    assert_eq!(
        actions(&h.db).await,
        [
            "plugin.uploaded",
            "plugin.key_pinned",
            "plugin.installed",
            "plugin.disabled",
            "plugin.enabled",
            "plugin.uninstalled",
            "plugin.upload_rejected",
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn bad_uploads_are_refused_and_audited(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = package(ID, &key, "");

    // Only the two fields, each once.
    for fields in [
        vec![("package", bytes.as_slice())],
        vec![
            ("package", bytes.as_slice()),
            ("signature", signature.as_bytes()),
            ("extra", b"x".as_slice()),
        ],
        vec![
            ("package", bytes.as_slice()),
            ("package", bytes.as_slice()),
            ("signature", signature.as_bytes()),
        ],
    ] {
        let res = send(&h.app, upload_request(&owner, multipart(&fields))).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    }
    // Not multipart at all.
    let res = send(&h.app, form("/admin/plugins", "package=x", &owner)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // Not a package; a signature by someone else.
    let res = upload(&h, &owner, b"not a zip", &signature).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let res = upload(&h, &owner, &bytes, &Key::new(2).sign(&bytes)).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(res.body.contains("signature"), "{}", res.body);

    // Migrations without asking for storage.
    let manifest_text = manifest(ID, &key, "");
    let confused = testing::zip(&[
        ("plugin.toml", manifest_text.as_bytes()),
        ("plugin.wasm", &hello_component()),
        ("migrations/0001_t.sql", b"CREATE TABLE t (x int);"),
    ]);
    let res = upload(&h, &owner, &confused, &key.sign(&confused)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("storage"), "{}", res.body);

    // A component that isn't a plugin.
    let manifest_text = manifest(ID, &key, "");
    let fake = testing::zip(&[
        ("plugin.toml", manifest_text.as_bytes()),
        ("plugin.wasm", testing::COMPONENT),
    ]);
    let res = upload(&h, &owner, &fake, &key.sign(&fake)).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(res.body.contains("component"), "{}", res.body);

    let rejected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'plugin.upload_rejected'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    // Every refusal above: four malformed requests, four bad packages.
    assert_eq!(rejected, 8);
    assert_eq!(h.plugins.status(ID), Status::Stopped);

    // Too many waiting.
    let account: i64 = sqlx::query_scalar("SELECT id FROM core.accounts LIMIT 1")
        .fetch_one(&h.db)
        .await
        .unwrap();
    for _ in 0..tether_web::plugins::MAX_PENDING_UPLOADS {
        tether_db::plugins::insert_upload(
            &h.db,
            "other.plugin",
            "1.0.0",
            b"x",
            "x",
            tether_db::accounts::AccountId(account),
        )
        .await
        .unwrap();
    }
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("Too many uploads"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn discarding_an_upload(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (bytes, signature) = package(ID, &Key::new(1), "");
    let review = upload(&h, &owner, &bytes, &signature)
        .await
        .location()
        .to_owned();
    let res = send(&h.app, form(&format!("{review}/discard"), "", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        page(&h, &review, &owner).await.status,
        StatusCode::NOT_FOUND
    );
    // Discarding doesn't pin anything.
    assert!(
        tether_db::plugin_keys::get(&h.db, ID)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        actions(&h.db).await,
        ["plugin.uploaded", "plugin.upload_discarded"]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn startup_loads_enabled_plugins_and_checks_them(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    install(&h, &owner, &Key::new(1)).await;

    let fresh = || {
        Plugins::new(
            tether_plugins::host::Host::new(std::sync::Arc::new(
                tether_plugins::Runtime::new().unwrap(),
            ))
            .unwrap(),
            test_key(),
        )
    };
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert_eq!(restarted.status(ID), Status::Running);
    assert_eq!(renders(&restarted).await, "Hello");

    // Disabled plugins stay off.
    send(&h.app, form("/admin/plugins/nmu.hello/disable", "", &owner)).await;
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert_eq!(restarted.status(ID), Status::Stopped);

    // A package changed in the database doesn't load: a flipped byte...
    sqlx::query(
        "UPDATE core.plugins SET enabled = true, \
         package = set_byte(package, 100, get_byte(package, 100) # 1) WHERE id = $1",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert!(
        matches!(restarted.status(ID), Status::Failed(ref why) if why.contains("approved")),
        "{:?}",
        restarted.status(ID)
    );

    // ...or a whole package signed by someone else, even with its hash
    // updated to match: it isn't signed with the pinned key.
    let (other, other_sig) = package(ID, &Key::new(9), "");
    let other_hash: Vec<u8> = {
        use sha2::Digest;
        sha2::Sha256::digest(&other).to_vec()
    };
    sqlx::query(
        "UPDATE core.plugins SET package = $2, signature = $3, package_sha256 = $4 WHERE id = $1",
    )
    .bind(ID)
    .bind(&other)
    .bind(&other_sig)
    .bind(&other_hash)
    .execute(&h.db)
    .await
    .unwrap();
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert!(
        matches!(restarted.status(ID), Status::Failed(ref why) if why.contains("check out")),
        "{:?}",
        restarted.status(ID)
    );
    let detail = page(&h, "/admin/plugins", &owner).await;
    assert_eq!(detail.status, StatusCode::OK);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_lost_key_is_re_pinned_on_its_page(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (old, new) = (Key::new(1), Key::new(2));
    install(&h, &owner, &old).await;
    send(
        &h.app,
        form(
            "/admin/plugins/nmu.hello/uninstall",
            "confirmation=nmu.hello",
            &owner,
        ),
    )
    .await;

    let key_page = page(&h, "/admin/plugin-keys/nmu.hello", &owner).await;
    assert_eq!(key_page.status, StatusCode::OK);
    assert!(key_page.body.contains(&old.public()));
    assert!(
        page(&h, "/admin/plugins", &owner)
            .await
            .body
            .contains("Pinned keys")
    );

    let body = |confirmation: &str| {
        format!(
            "expected_old={}&new_key={}&confirmation={confirmation}",
            urlencode(&old.public()),
            urlencode(&new.public())
        )
    };
    let res = send(
        &h.app,
        form("/admin/plugin-keys/nmu.hello", &body("nope"), &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains(&new.public()), "keeps what was typed");
    let res = send(
        &h.app,
        form("/admin/plugin-keys/nmu.hello", &body(ID), &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // The new key's package now installs as pinned.
    let (bytes, signature) = package(ID, &new, "");
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let review = page(&h, res.location(), &owner).await.body;
    assert!(review.contains("Signed with the pinned key"), "{review}");
    assert_eq!(
        page(&h, "/admin/plugin-keys/unknown.plugin", &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_re_pin_stops_packages_signed_with_the_old_key(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (old, new) = (Key::new(1), Key::new(2));
    install(&h, &owner, &old).await;
    tether_web::plugins::repin_key(
        &h.db,
        tether_db::audit::Actor::System,
        ID,
        &old.public(),
        &new.public(),
        ID,
    )
    .await
    .unwrap();
    // Still running until it is loaded again...
    assert_eq!(h.plugins.status(ID), Status::Running);
    send(&h.app, form("/admin/plugins/nmu.hello/disable", "", &owner)).await;
    send(&h.app, form("/admin/plugins/nmu.hello/enable", "", &owner)).await;
    // ...then refused: it isn't signed with the pinned key any more.
    assert!(
        matches!(h.plugins.status(ID), Status::Failed(ref why) if why.contains("different key")),
        "{:?}",
        h.plugins.status(ID)
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_ids_never_collide_with_admin_routes(db: PgPool) {
    // Words that are (or were) route segments are fine plugin ids: every
    // plugin's page and actions still reach it.
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    for id in ["upload", "uploads", "keys"] {
        let (bytes, signature) = package(id, &key, "");
        let res = upload(&h, &owner, &bytes, &signature).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{id}: {}", res.body);
        let res = send(
            &h.app,
            form(&format!("{}/approve", res.location()), "", &owner),
        )
        .await;
        assert_eq!(res.location(), format!("/admin/plugins/{id}"), "{id}");

        assert_eq!(
            page(&h, &format!("/admin/plugins/{id}"), &owner)
                .await
                .status,
            StatusCode::OK,
            "{id}"
        );
        let res = send(
            &h.app,
            form(&format!("/admin/plugins/{id}/disable"), "", &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{id}");
        assert_eq!(h.plugins.status(id), Status::Stopped, "{id}");
        let res = send(
            &h.app,
            form(
                &format!("/admin/plugins/{id}/uninstall"),
                &format!("confirmation={id}"),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{id}");
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn one_upload_is_checked_at_a_time(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (bytes, signature) = package(ID, &Key::new(1), "");
    let busy = h.plugins.upload_permit().unwrap();
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    drop(busy);
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}
