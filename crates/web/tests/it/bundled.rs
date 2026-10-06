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
         [[navigation]]\nlabel = \"Hello\"\npath = \"\"\nsection = \"industry\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n"
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
    // Nothing installed yet; the app waits under Included with Tether.
    assert!(res.body.contains("None yet."), "{}", res.body);
    let included = res.body.find("Included with Tether").unwrap();
    let offer = res
        .body
        .find(&format!("/admin/plugin-bundled/{ID}"))
        .unwrap();
    assert!(included < offer, "{}", res.body);
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

    // Installed: nothing to approve again, and nothing left to include.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(!res.body.contains("Review and install"));
    assert!(!res.body.contains("Included with Tether"), "{}", res.body);
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
    let _lock = guests_lock(&root);
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

// ---- rebuilds: the same version, another package ---------------------------

/// A component built for another version of the app interface: the limits
/// test guest exports its own world, not `render`.
fn mismatched_component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest"))
        .clone()
}

/// A bundled package of app `id` with `extra` manifest lines and this
/// component.
fn package_of(id: &str, version: &str, permissions: &str, extra: &str, wasm: &[u8]) -> Vec<u8> {
    let manifest = format!(
        "[plugin]\nid = \"{id}\"\nname = \"Hello {id}\"\nversion = \"{version}\"\n\
         host_api = \"1\"\n\n[permissions]\n{permissions}\n{extra}"
    );
    testing::zip(&[("plugin.toml", manifest.as_bytes()), ("plugin.wasm", wasm)])
}

async fn sha_of(db: &PgPool, id: &str) -> String {
    let sha: Vec<u8> = sqlx::query_scalar("SELECT package_sha256 FROM core.plugins WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap();
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

/// `plugin.upgraded` entries: (actor account, details), oldest first.
async fn upgrades(db: &PgPool) -> Vec<(Option<i64>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT actor_account_id, details FROM core.audit_log \
         WHERE action = 'plugin.upgraded' ORDER BY id",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

const VIEW: &str = "view = \"See the hello page\"\n";
const VIEW_RULE: &str = "[[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"\"\npermission = \"view\"\n";

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_rebuild_that_asks_for_nothing_new_is_applied_at_startup(db: PgPool) {
    // Approved from one image, whose build doesn't fit this Tether's app
    // interface any more.
    let broken = package_of(ID, "1.0.0", VIEW, VIEW_RULE, &mismatched_component());
    let h = harness_with_bundled(db.clone(), vec![broken.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let res = approve(&h, &owner, &broken, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(matches!(h.plugins.status(ID), Status::Incompatible(_)));

    // Said plainly, with nothing to review: install a version built for
    // this Tether. The error itself is there for admins; uninstalling
    // isn't the fix.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(
        res.body.contains("Built for a different version of Tether"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Install a version built for this Tether"));
    assert!(res.body.contains("The error</summary>"));
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(
        res.body.contains("Built for a different version of Tether"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Install a version built for this Tether"));
    assert!(res.body.contains("The error</summary>"));
    assert!(res.body.contains("Uninstalling doesn"));

    // The next image rebuilt it, at the same version, asking for exactly
    // the same: offered as a rebuild until Tether starts...
    let rebuilt = package_of(ID, "1.0.0", VIEW, VIEW_RULE, &hello_component());
    let h = harness_with_bundled(db.clone(), vec![rebuilt.clone()]).await;
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(
        res.body.contains("Rebuilt with this Tether"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Review rebuild"));
    let res = page(&h, &format!("/admin/plugin-bundled/{ID}"), &owner).await;
    assert!(
        res.body.contains("rebuilt with this Tether"),
        "{}",
        res.body
    );

    // ...which applies it, as the system, and runs it.
    h.plugins.start(&h.db).await.unwrap();
    assert_eq!(h.plugins.status(ID), Status::Running);
    assert_eq!(sha_of(&h.db, ID).await, sha256_hex(&rebuilt));
    assert_eq!(
        stored(&h.db).await,
        (
            "bundled".to_owned(),
            None,
            Some("bundled".to_owned()),
            "1.0.0".to_owned()
        )
    );
    let audited = upgrades(&h.db).await;
    assert_eq!(audited.len(), 1);
    let (actor, details) = &audited[0];
    assert_eq!(*actor, None);
    assert_eq!(details["reason"], "bundled_rebuild");
    assert_eq!(details["from"], "1.0.0");
    assert_eq!(details["to"], "1.0.0");
    assert_eq!(details["sha256"], sha256_hex(&rebuilt));
    let declared: Vec<String> =
        sqlx::query_scalar("SELECT permission FROM core.plugin_permissions ORDER BY 1")
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(declared, ["plugin.tether.hello.view"]);

    // Nothing left to offer; starting again changes nothing.
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(
        !res.body.contains("Rebuilt with this Tether"),
        "{}",
        res.body
    );
    let h = harness_with_bundled(db.clone(), vec![rebuilt.clone()]).await;
    h.plugins.start(&h.db).await.unwrap();
    assert_eq!(upgrades(&h.db).await.len(), 1);

    // An admin rolling back from it is a choice a restart doesn't undo:
    // offered, not applied.
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
    assert_eq!(sha_of(&h.db, ID).await, sha256_hex(&broken));
    let h = harness_with_bundled(db, vec![rebuilt.clone()]).await;
    h.plugins.start(&h.db).await.unwrap();
    assert_eq!(sha_of(&h.db, ID).await, sha256_hex(&broken));
    assert_eq!(upgrades(&h.db).await.len(), 1);
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(res.body.contains("Review the rebuild"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_rebuild_that_asks_for_more_waits_for_review(db: PgPool) {
    let v1 = package_of(ID, "1.0.0", VIEW, "", &hello_component());
    let h = harness_with_bundled(db.clone(), vec![v1.clone()]).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let res = approve(&h, &owner, &v1, "none").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let base = sha256_hex(&v1);

    // Rebuilt at the same version but opening its pages to whoever holds
    // `view` (admins only before): the review would list that, so Tether
    // doesn't apply it itself. Nor one with a new permission, nor a newer
    // version with a new scope.
    let rebuilds = [
        package_of(ID, "1.0.0", VIEW, VIEW_RULE, &hello_component()),
        package_of(
            ID,
            "1.0.0",
            "view = \"See the hello page\"\nwave = \"Wave back\"\n",
            "",
            &hello_component(),
        ),
        package_of(
            ID,
            "1.1.0",
            VIEW,
            "[capabilities.esi]\ndata_source = [\"esi-industry.read_corporation_mining.v1\"]\n",
            &hello_component(),
        ),
    ];
    for rebuilt in &rebuilds {
        let h = harness_with_bundled(db.clone(), vec![rebuilt.clone()]).await;
        h.plugins.start(&h.db).await.unwrap();
        assert_eq!(h.plugins.status(ID), Status::Running);
        assert_eq!(sha_of(&h.db, ID).await, base);
        assert!(upgrades(&h.db).await.is_empty());
        let res = page(&h, "/admin/plugins", &owner).await;
        assert!(
            res.body.contains("Rebuilt with this Tether")
                || res.body.contains("1.1.0</span> available"),
            "{}",
            res.body
        );
    }

    // Reviewed and approved by an admin, at the same version.
    let rebuilt = rebuilds[1].clone();
    let h = harness_with_bundled(db, vec![rebuilt.clone()]).await;
    h.plugins.start(&h.db).await.unwrap();
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(res.body.contains("Review rebuild"), "{}", res.body);
    let res = page(&h, &format!("/admin/plugin-bundled/{ID}"), &owner).await;
    assert!(
        res.body.contains("rebuilt with this Tether"),
        "{}",
        res.body
    );
    assert!(res.body.contains("plugin.tether.hello.wave"));
    let res = approve(&h, &owner, &rebuilt, &base).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(sha_of(&h.db, ID).await, sha256_hex(&rebuilt));
    assert_eq!(stored(&h.db).await.3, "1.0.0");
    let audited = upgrades(&h.db).await;
    assert_eq!(audited.len(), 1);
    assert!(audited[0].0.is_some());
    assert_eq!(audited[0].1["reason"], "bundled_rebuild");
    // The package it replaced can be put back.
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(res.body.contains("Roll back to"), "{}", res.body);
    // The same package again is nothing to install.
    let res = approve(&h, &owner, &rebuilt, &sha256_hex(&rebuilt)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_signed_install_of_a_bundled_id_is_never_touched(db: PgPool) {
    // Installed from a publisher's signed package before this Tether
    // bundled the id.
    let h = harness(db.clone(), true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let (bytes, signature) = signed(&Key::new(7));
    install_package(&h, &owner, &bytes, &signature).await;
    let before = sha_of(&h.db, ID).await;

    // Bundled at the same version, asking for the same: not a rebuild of
    // it, so it's neither offered as one nor applied.
    let twin = package_of(ID, "9.0.0", "", "", &hello_component());
    let h = harness_with_bundled(db, vec![twin]).await;
    h.plugins.start(&h.db).await.unwrap();
    assert_eq!(sha_of(&h.db, ID).await, before);
    assert_eq!(stored(&h.db).await.0, "signed");
    assert!(upgrades(&h.db).await.is_empty());
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(
        !res.body.contains("Rebuilt with this Tether"),
        "{}",
        res.body
    );
    assert!(!res.body.contains("Review rebuild"));
    let res = page(&h, &format!("/admin/plugins/{ID}"), &owner).await;
    assert!(!res.body.contains("Review the rebuild"), "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn approve_all_applies_each_in_turn_and_stops_at_a_failure(db: PgPool) {
    const A: &str = "tether.hello-a";
    const B: &str = "tether.hello-b";
    const C: &str = "tether.hello-c";
    let wasm = hello_component();
    let v1: Vec<Vec<u8>> = [A, B, C]
        .iter()
        .map(|id| package_of(id, "1.0.0", "", "", &wasm))
        .collect();
    let h = harness_with_bundled(db.clone(), v1.clone()).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    for (id, bytes) in [A, B, C].iter().zip(&v1) {
        let res = send(
            &h.app,
            form(
                &format!("/admin/plugin-bundled/{id}/approve"),
                &format!("package={}&reviewed=none", sha256_hex(bytes)),
                &owner,
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    }
    let wave = "wave = \"Wave back\"\n";
    let next = vec![
        package_of(A, "1.0.0", wave, "", &wasm),
        package_of(B, "2.0.0", wave, "", &wasm),
        package_of(C, "2.0.0", wave, "", &wasm),
    ];
    let h = harness_with_bundled(db, next.clone()).await;
    h.plugins.start(&h.db).await.unwrap();
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(
        res.body.contains("Approve all included updates"),
        "{}",
        res.body
    );
    // Each app's own review stays.
    assert!(res.body.contains(&format!("/admin/plugin-bundled/{A}")));

    // One page, each app's changes.
    let res = page(&h, "/admin/plugin-bundled-updates", &owner).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    for id in [A, B, C] {
        assert!(
            res.body.contains(&format!("plugin.{id}.wave")),
            "{}",
            res.body
        );
    }
    assert!(res.body.contains("Rebuilt with this Tether"));
    let token = |id: &str, bytes: &[u8], v1: &[u8]| {
        format!("app={id}%3A{}%3A{}", sha256_hex(bytes), sha256_hex(v1))
    };

    // Only what the page offers: not an app it doesn't list, nor one twice.
    for body in [
        format!(
            "{}&app={ID}%3A{}%3Anone",
            token(A, &next[0], &v1[0]),
            "0".repeat(64)
        ),
        [token(A, &next[0], &v1[0]), token(A, &next[0], &v1[0])].join("&"),
    ] {
        let res = send(
            &h.app,
            form("/admin/plugin-bundled-updates/approve", &body, &owner),
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
        assert_eq!(sha_of(&h.db, A).await, sha256_hex(&v1[0]));
    }

    // B's review is stale (it showed another package): A is upgraded, B
    // isn't, and C after it isn't tried.
    let body = [
        token(A, &next[0], &v1[0]),
        format!("app={B}%3A{}%3A{}", "0".repeat(64), sha256_hex(&v1[1])),
        token(C, &next[2], &v1[2]),
    ]
    .join("&");
    let res = send(
        &h.app,
        form("/admin/plugin-bundled-updates/approve", &body, &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(
        res.body.contains("Upgraded: Hello tether.hello-a."),
        "{}",
        res.body
    );
    assert!(
        res.body.contains("Hello tether.hello-b wasn"),
        "{}",
        res.body
    );
    assert!(res.body.contains("The ones after it weren"), "{}", res.body);
    assert_eq!(sha_of(&h.db, A).await, sha256_hex(&next[0]));
    assert_eq!(sha_of(&h.db, B).await, sha256_hex(&v1[1]));
    assert_eq!(sha_of(&h.db, C).await, sha256_hex(&v1[2]));

    // The rest, as shown again.
    let body = [token(B, &next[1], &v1[1]), token(C, &next[2], &v1[2])].join("&");
    let res = send(
        &h.app,
        form("/admin/plugin-bundled-updates/approve", &body, &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.location(), "/admin/plugins");
    for (id, bytes) in [A, B, C].iter().zip(&next) {
        assert_eq!(sha_of(&h.db, id).await, sha256_hex(bytes));
        assert_eq!(h.plugins.status(id), Status::Running);
    }
    // One audited upgrade each, by the admin.
    let audited = upgrades(&h.db).await;
    assert_eq!(audited.len(), 3);
    assert!(audited.iter().all(|(actor, _)| actor.is_some()));
    let res = page(&h, "/admin/plugins", &owner).await;
    assert!(!res.body.contains("Approve all included updates"));
}
