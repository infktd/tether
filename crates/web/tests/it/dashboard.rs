//! The Dashboard: the pilot's characters, with system admins' panel on
//! Administration instead, and no widgets from apps.

use std::sync::OnceLock;

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_plugins::testing::{self, Key};

use crate::common::*;

const ID: &str = "acme.widgets";
const CHRIBBA: &str = "196379789:Chribba";
const MITTANI: &str = "443630591:The Mittani";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-pages"))
        .clone()
}

/// An app declaring widgets, as packages built before the Dashboard became
/// the character audit did.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Widgets\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See the pages\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n\n[[pages]]\npath = \"values\"\npermission = \"view\"\n\n\
         [[pages]]\npath = \"failed\"\npermission = \"view\"\n\n\
         [[widgets]]\ntitle = \"Ore\"\npath = \"values\"\n\n\
         [[widgets]]\ntitle = \"Secret\"\npath = \"admin/secret\"\n\n\
         [[widgets]]\ntitle = \"Broken\"\npath = \"failed\"\n",
        key.public()
    );
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn system_admins_get_the_system_panel_on_administration(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;

    // At the top of Administration's overview, not on the Dashboard, which
    // is for pilots.
    let overview = page(&h, "/admin", &owner).await.body;
    let panel_at = overview
        .find(r#"hx-get="/admin/system/summary""#)
        .expect("Health's verdict");
    assert!(
        panel_at < overview.find("admin-tile").unwrap(),
        "{overview}"
    );
    let dashboard = page(&h, "/dashboard", &owner).await.body;
    assert!(!dashboard.contains("/system"), "{dashboard}");
    assert!(!dashboard.contains("verdict"), "{dashboard}");
    let panel = page(&h, "/admin/system/summary", &owner).await;
    assert_eq!(panel.status, StatusCode::OK, "{}", panel.body);
    // The verdict, its heading the link to Health, and whatever isn't
    // simply working: here Discord, not set up.
    assert!(panel.body.contains("verdict"), "{}", panel.body);
    assert!(
        panel.body.contains(r#"href="/admin/system""#),
        "{}",
        panel.body
    );
    assert!(panel.body.contains("Not set up"), "{}", panel.body);
    assert!(!panel.body.contains("<html"), "a fragment");

    assert_eq!(
        page(&h, "/dashboard/system", &owner).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        page(&h, "/admin/system/summary", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
}

/// The Dashboard is the pilot's characters (Jay, 2026-10-06): apps add no
/// widgets to it. A package that still declares them installs and runs,
/// and the Dashboard doesn't load them.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn widgets_are_read_and_ignored(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    install(&h, &owner).await;
    let dashboard = page(&h, "/dashboard", &owner).await;
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    assert!(!dashboard.body.contains("widget"), "{}", dashboard.body);
    assert!(!dashboard.body.contains(ID), "{}", dashboard.body);
    assert_eq!(
        page(&h, &format!("/dashboard/widgets/{ID}/0"), &owner)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let values = page(&h, &format!("/plugins/{ID}/values"), &owner).await;
    assert_eq!(values.status, StatusCode::OK, "{}", values.body);
}
