//! Apps bundled into Tether's image: installed and upgraded after the
//! usual review with no signature and no pinned key, their ids reserved
//! against every other source, and rolled back like any other app.

use std::sync::OnceLock;

use crate::common::*;
use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};
use tether_web::plugins::{Plugins, Status};

const ID: &str = "tether.hello";

fn hello_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("hello-plugin"))
        .clone()
}

/// The hello plugin as the image build packages a bundled app: no
/// `[publisher]`, no signature.
fn bundled(version: &str, permissions: &str) -> Vec<u8> {
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Hello\"\nversion = \"{version}\"\nhost_api = \"1\"\n\
         description = \"Says hello\"\n\n[permissions]\n{permissions}\n\
         [[navigation]]\nlabel = \"Hello\"\npath = \"\"\nsection = \"industry\"\n"
    );
    testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &hello_component()),
    ])
}

/// The same id, signed by someone.
fn signed(key: &Key) -> (Vec<u8>, String) {
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Hello\"\nversion = \"9.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n",
        key.public()
    );
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &hello_component()),
    ]);
    let signature = key.sign(&bytes);
    (bytes, signature)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn stored(db: &PgPool) -> (String, Option<String>, Option<String>, String) {
    sqlx::query_as(
        "SELECT origin, signature, previous_origin, version FROM core.plugins WHERE id = $1",
    )
    .bind(ID)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn approve(h: &Harness, owner: &str, package: &[u8], reviewed: &str) -> Res {
    send(
        &h.app,
        form(
            &format!("/admin/plugin-bundled/{ID}/approve"),
            &format!("package={}&reviewed={reviewed}", sha256_hex(package)),
            owner,
        ),
    )
    .await
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_bundled_app_installs_after_review_with_no_signature(db: PgPool) {
    let v1 = bundled("1.0.0", "view = \"See the hello page\"\n");
    let h = harness_with_bundled(db, vec![v1.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;

    let res = page(&h, "/admin/plugins", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Included with Tether"), "{}", res.body);
    assert!(res.body.contains(&format!("/admin/plugin-bundled/{ID}")));
    assert!(res.body.contains("Review and install"));

    // The same review as any app: what it asks for, no publisher key.
    let res = page(&h, &format!("/admin/plugin-bundled/{ID}"), &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Comes with Tether"));
    assert!(res.body.contains("plugin.tether.hello.view"));
    assert!(!res.body.contains("Publisher key"));
    assert!(res.body.contains(&sha256_hex(&v1)));

    // Only the package the review showed.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-bundled/{ID}/approve"),
            &format!("package={}&reviewed=none", "0".repeat(64)),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("Tether was updated"));
    // And only with what was installed when it was shown.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-bundled/{ID}/approve"),
            &format!("package={}", sha256_hex(&v1)),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(h.plugins.status(ID), Status::Stopped);

    let res = approve(&h, &owner, &v1, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), format!("/admin/plugins/{ID}"));
    assert_eq!(h.plugins.status(ID), Status::Running);
    assert_eq!(
        stored(&h.db).await,
        ("bundled".to_owned(), None, None, "1.0.0".to_owned())
    );
    // No key pinned, and the audit log says where it came from.
    let pins: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_keys")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(pins, 0);
    let details: serde_json::Value =
        sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = 'plugin.installed'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(details["origin"], "bundled");
    assert_eq!(details["key"], serde_json::Value::Null);
    assert_eq!(details["sha256"], sha256_hex(&v1));

    // Its sidebar link is in the section it names.
    let nav = h.plugins.navigation();
    assert_eq!(nav[0].section, "industry");

    // Installed: nothing to approve again.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(!res.body.contains("Review and install"));
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("Included with Tether"));
    assert!(!res.body.contains("Publisher key"));

    // Only a bundled package goes without a signature.
    let unsigned = sqlx::query("UPDATE core.plugins SET origin = 'signed' WHERE id = $1")
        .bind(ID)
        .execute(&h.db)
        .await;
    assert!(unsigned.is_err());

    // It loads at startup with no key pinned; a changed package doesn't.
    let fresh = || {
        Plugins::new(
            tether_plugins::host::Host::new(std::sync::Arc::new(
                tether_plugins::Runtime::new().unwrap(),
            ))
            .unwrap(),
            tether_web::plugin_services::Deps {
                db: h.db.clone(),
                esi: h.esi.clone(),
                vault: h.vault.clone(),
                discord: h.discord.clone(),
                key: test_key(),
                public_url: SITE.to_owned(),
                snapshots: None,
                github: None,
                bundled: std::sync::Arc::default(),
            },
        )
    };
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert_eq!(restarted.status(ID), Status::Running);
    sqlx::query(
        "UPDATE core.plugins SET package = set_byte(package, 100, get_byte(package, 100) # 1) \
         WHERE id = $1",
    )
    .bind(ID)
    .execute(&h.db)
    .await
    .unwrap();
    let restarted = fresh();
    restarted.start(&h.db).await.unwrap();
    assert!(
        matches!(restarted.status(ID), Status::Failed(why) if why.contains("isn't the one that was approved"))
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_bundled_apps_id_is_reserved(db: PgPool) {
    let v1 = bundled("1.0.0", "");
    let h = harness_with_bundled(db, vec![v1.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let key = Key::new(1);
    let (bytes, signature) = signed(&key);

    // Uploaded (or fetched from GitHub, which checks it the same way).
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("comes with Tether"), "{}", res.body);
    let uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_uploads")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(uploads, 0);

    // One waiting from before this Tether bundled it isn't approved.
    let account: i64 = sqlx::query_scalar("SELECT min(id) FROM core.accounts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let upload_id = tether_db::plugins::insert_upload(
        &h.db,
        ID,
        "9.0.0",
        &bytes,
        &signature,
        tether_db::accounts::AccountId(account),
        None,
    )
    .await
    .unwrap();
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-uploads/{upload_id}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("comes with Tether"));
    assert_eq!(h.plugins.status(ID), Status::Stopped);

    // Installed, its updates can't be pointed at a repository.
    let res = approve(&h, &owner, &v1, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/source"),
            "source=https%3A%2F%2Fgithub.com%2Fsomeone%2Fhello",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    let source: Option<String> = sqlx::query_scalar("SELECT source FROM core.plugins")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(source, None);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_bundled_install_stays_reserved_when_tether_stops_bundling_it(db: PgPool) {
    let v1 = bundled("1.0.0", "");
    let h = harness_with_bundled(db.clone(), vec![v1.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let res = approve(&h, &owner, &v1, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);

    // A Tether that doesn't bundle it (or couldn't read it): no key is
    // pinned, but a signed package still can't take the id over.
    let h = harness_with_bundled(db, Vec::new()).await;
    let (bytes, signature) = signed(&Key::new(3));
    let res = upload(&h, &owner, &bytes, &signature).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("comes with Tether"), "{}", res.body);
    let pins: i64 = sqlx::query_scalar("SELECT count(*) FROM core.plugin_keys")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(pins, 0);

    // Nor one waiting from before, at approval.
    let account: i64 = sqlx::query_scalar("SELECT min(id) FROM core.accounts")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let upload_id = tether_db::plugins::insert_upload(
        &h.db,
        ID,
        "9.0.0",
        &bytes,
        &signature,
        tether_db::accounts::AccountId(account),
        None,
    )
    .await
    .unwrap();
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugin-uploads/{upload_id}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(stored(&h.db).await.3, "1.0.0");

    // And its updates can't be pointed at a repository.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/source"),
            "source=https%3A%2F%2Fgithub.com%2Fsomeone%2Fhello",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_newer_tether_offers_the_update_and_it_rolls_back(db: PgPool) {
    let v1 = bundled("1.0.0", "view = \"See the hello page\"\n");
    let h = harness_with_bundled(db.clone(), vec![v1.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let res = approve(&h, &owner, &v1, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let base = sha256_hex(&v1);

    // A newer image, with version 2 asking for another permission.
    let v2 = bundled(
        "2.0.0",
        "view = \"See the hello page\"\nwave = \"Wave back\"\n",
    );
    let h = harness_with_bundled(db, vec![v2.clone()]).await;
    h.plugins.start(&h.db).await.unwrap();
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(res.body.contains("Review update"), "{}", res.body);
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(res.body.contains("Review version"), "{}", res.body);

    let res = page(&h, &format!("/admin/plugin-bundled/{ID}"), &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("What changes"));
    assert!(res.body.contains("plugin.tether.hello.wave"));
    assert!(res.body.contains("Approve and upgrade"));

    let res = approve(&h, &owner, &v2, &base).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        stored(&h.db).await,
        (
            "bundled".to_owned(),
            None,
            Some("bundled".to_owned()),
            "2.0.0".to_owned()
        )
    );
    assert_eq!(h.plugins.status(ID), Status::Running);
    // Nothing newer now.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(!res.body.contains("Review update"));

    // One step back, with no key to check.
    let res = send(
        &h.app,
        form(
            &format!("/admin/plugins/{ID}/rollback"),
            &format!("confirmation={ID}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        stored(&h.db).await,
        ("bundled".to_owned(), None, None, "1.0.0".to_owned())
    );
    assert_eq!(h.plugins.status(ID), Status::Running);
    let declared: Vec<String> =
        sqlx::query_scalar("SELECT permission FROM core.plugin_permissions ORDER BY 1")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(declared, ["plugin.tether.hello.view"]);
    // And this Tether's version is offered again.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(res.body.contains("Review update"));
}

/// The first-party apps as the image bundles them: `scripts/bundle-apps.sh`
/// (needs zip), built into the test guests' target directory.
fn first_party_bundle() -> tether_web::bundled::Bundled {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::env::temp_dir().join(format!("tether-apps-{}", std::process::id()));
    let output = std::process::Command::new(root.join("scripts/bundle-apps.sh"))
        .arg(&out)
        .env("CARGO_TARGET_DIR", root.join("target/test-guests"))
        .output()
        .expect("running scripts/bundle-apps.sh");
    assert!(
        output.status.success(),
        "bundling: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundled = tether_web::bundled::Bundled::read_dir(&out);
    std::fs::remove_dir_all(&out).unwrap();
    bundled
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_first_party_app_is_bundled_and_installs(db: PgPool) {
    let bundled = first_party_bundle();
    let ids: Vec<String> = bundled
        .all()
        .iter()
        .map(|a| a.package.manifest.plugin.id.clone())
        .collect();
    let dirs = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"))
        .unwrap()
        .count();
    assert_eq!(ids.len(), dirs, "{ids:?}");
    for app in bundled.all() {
        assert!(app.package.manifest.publisher.is_none());
    }
    let packages: Vec<Vec<u8>> = bundled.all().iter().map(|a| a.bytes.clone()).collect();

    let h = harness_with_bundled(db, packages.clone()).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    for (id, bytes) in ids.iter().zip(&packages) {
        let res = page(&h, &format!("/admin/plugin-bundled/{id}"), &owner).await;
        assert_eq!(res.status, StatusCode::OK, "{id}: {}", res.body);
        let res = send(
            &h.app,
            form(
                &format!("/admin/plugin-bundled/{id}/approve"),
                &format!("package={}&reviewed=none", sha256_hex(bytes)),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{id}: {}", res.body);
        assert_eq!(h.plugins.status(id), Status::Running, "{id}");
    }
}

#[test]
fn bundled_apps_are_read_from_their_directory() {
    let dir = std::env::temp_dir().join(format!("tether-bundled-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tether.hello-1.0.0.zip"), bundled("1.0.0", "")).unwrap();
    std::fs::write(dir.join("broken.zip"), b"not a zip").unwrap();
    std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
    let read = tether_web::bundled::Bundled::read_dir(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(read.reserves(ID));
    assert_eq!(read.all().len(), 1);
    assert_eq!(
        read.get(ID).unwrap().package.manifest.plugin.version,
        "1.0.0"
    );
    // No directory: none.
    let none = tether_web::bundled::Bundled::read_dir(&dir);
    assert!(none.all().is_empty());
}
