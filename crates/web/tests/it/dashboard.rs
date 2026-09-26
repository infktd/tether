//! The Dashboard (AA's): admin panels for system admins, and widgets
//! plugins add, each shown to whoever may open its page.

use std::sync::OnceLock;

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::StateId;
use tether_db::permissions::Grantee;
use tether_plugins::testing::{self, Key};

use crate::common::*;

const ID: &str = "nmu.widgets";
const CHRIBBA: &str = "196379789:Chribba";
const MITTANI: &str = "443630591:The Mittani";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-pages"))
        .clone()
}

/// Widgets for `values` (needs `view`), `admin/secret` (admins only) and
/// `failed` (the plugin errors).
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Widgets\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See the pages\"\n\n\
         [[pages]]\npath = \"values\"\npermission = \"view\"\n\n\
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
async fn system_admins_get_the_admin_panels(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;

    let dashboard = page(&h, "/dashboard", &owner).await;
    assert!(
        dashboard.body.contains(r#"hx-get="/dashboard/system""#),
        "{}",
        dashboard.body
    );
    let panel = page(&h, "/dashboard/system", &owner).await;
    assert_eq!(panel.status, StatusCode::OK, "{}", panel.body);
    assert!(
        panel.body.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        panel.body
    );
    assert!(panel.body.contains("Task Queue"), "{}", panel.body);
    assert!(panel.body.contains("ESI"), "{}", panel.body);
    assert!(!panel.body.contains("<html"), "a fragment");

    assert!(
        !page(&h, "/dashboard", &pilot)
            .await
            .body
            .contains("/dashboard/system")
    );
    assert_eq!(
        page(&h, "/dashboard/system", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_widgets_follow_their_pages(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;
    install(&h, &owner).await;
    let ore = format!(r#"hx-get="/dashboard/widgets/{ID}/0""#);
    let secret = format!(r#"hx-get="/dashboard/widgets/{ID}/1""#);

    // Without `view`, nothing: no widget, and the fragment is a 404.
    assert!(!page(&h, "/dashboard", &pilot).await.body.contains(&ore));
    let denied = page(&h, &format!("/dashboard/widgets/{ID}/0"), &pilot).await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);

    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(
            &h.db,
            "plugin.nmu.widgets.view",
            Grantee::State(StateId(state)),
        )
        .await
        .unwrap();
    }
    let dashboard = page(&h, "/dashboard", &pilot).await;
    assert!(dashboard.body.contains(&ore), "{}", dashboard.body);
    assert!(!dashboard.body.contains(&secret), "admins only");
    assert!(page(&h, "/dashboard", &owner).await.body.contains(&secret));

    let widget = page(&h, &format!("/dashboard/widgets/{ID}/0"), &pilot).await;
    assert_eq!(widget.status, StatusCode::OK, "{}", widget.body);
    assert!(widget.body.contains("Ore"), "{}", widget.body);
    assert!(
        widget
            .body
            .contains(&format!(r#"href="/plugins/{ID}/values""#)),
        "links to its page: {}",
        widget.body
    );
    // Sections only: tabs stay on the page.
    assert!(!widget.body.contains("first tab"), "{}", widget.body);
    assert!(!widget.body.contains("<html"), "a fragment");

    // A failing plugin costs its own widget only, and says so politely.
    let broken = page(&h, &format!("/dashboard/widgets/{ID}/2"), &pilot).await;
    assert_eq!(broken.status, StatusCode::OK, "{}", broken.body);
    assert!(broken.body.contains("couldn't load"), "{}", broken.body);
    assert!(
        !broken.body.contains("on fire"),
        "plugin error text stays in its log"
    );

    assert!(
        widget.body.contains("Viewing as The Mittani"),
        "watermarked"
    );

    let missing = page(&h, &format!("/dashboard/widgets/{ID}/9"), &pilot).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let no_plugin = page(&h, "/dashboard/widgets/no.such/0", &pilot).await;
    assert_eq!(no_plugin.status, StatusCode::NOT_FOUND);
    let not_a_number = page(&h, &format!("/dashboard/widgets/{ID}/x"), &pilot).await;
    assert_eq!(not_a_number.status, StatusCode::NOT_FOUND);

    // A widget is a page view: it shares the page's rate limit.
    loop {
        let res = page(&h, &format!("/plugins/{ID}/values"), &pilot).await;
        if res.status == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
        assert_eq!(res.status, StatusCode::OK);
    }
    let limited = page(&h, &format!("/dashboard/widgets/{ID}/0"), &pilot).await;
    assert!(limited.body.contains("couldn't load"), "{}", limited.body);

    // Signed out, the same for an installed plugin as for none.
    for uri in [
        format!("/dashboard/widgets/{ID}/0"),
        "/dashboard/widgets/no.such/0".to_owned(),
        format!("/dashboard/widgets/{ID}/x"),
    ] {
        assert_eq!(send(&h.app, get(&uri, &[])).await.location(), "/login");
    }
}
