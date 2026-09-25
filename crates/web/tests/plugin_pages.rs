#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Plugin pages: who may open them (404 for anyone who may not), how the
//! host draws them, plugin navigation, forms checked before the plugin
//! sees them, and plugin permissions granted like core ones.

mod common;

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use sqlx::PgPool;
use tether_core::states::StateId;
use tether_db::permissions::Grantee;
use tether_plugins::testing::{self, Key};

const ID: &str = "nmu.pages";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-pages"))
        .clone()
}

/// Every page but `admin/...` needs `view`; `admin/...` has no rule, so
/// it's for admins only.
async fn install(h: &Harness, owner: &str) {
    let key = Key::new(1);
    let mut manifest = format!(
        "[plugin]\nid = \"{ID}\"\nname = \"Pages\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See the pages\"\n\n\
         [[navigation]]\nlabel = \"Values\"\npath = \"values\"\n\n\
         [[navigation]]\nlabel = \"Secret\"\npath = \"admin/secret\"\n",
        key.public()
    );
    for path in ["values", "form", "failed", "crash", "query", "missing"] {
        manifest.push_str(&format!(
            "\n[[pages]]\npath = \"{path}\"\npermission = \"view\"\n"
        ));
    }
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    let at = install_package(h, owner, &bytes, &key.sign(&bytes)).await;
    assert_eq!(at, format!("/admin/plugins/{ID}"));
}

async fn grant_view(db: &PgPool) {
    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(db, "plugin.nmu.pages.view", Grantee::State(StateId(state)))
            .await
            .unwrap();
    }
}

async fn setup(db: PgPool) -> (Harness, String, String) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "196379789:Chribba").await;
    let pilot = log_in_as(&h, "443630591:The Mittani", None).await;
    install(&h, &owner).await;
    (h, owner, pilot)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn only_those_allowed_learn_a_page_exists(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    let uri = "/plugins/nmu.pages/values";
    assert_eq!(send(&h.app, get(uri, &[])).await.location(), "/login");

    // Without the permission, the same 404 as for nothing at all.
    let denied = page(&h, uri, &pilot).await;
    let nothing = page(&h, "/plugins/no.such.plugin", &pilot).await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    assert_eq!(nothing.status, StatusCode::NOT_FOUND);
    assert_eq!(denied.body, nothing.body);
    assert!(
        !page(&h, "/dashboard", &pilot).await.body.contains(uri),
        "no sidebar link"
    );

    grant_view(&h.db).await;
    assert_eq!(page(&h, uri, &pilot).await.status, StatusCode::OK);
    assert!(
        page(&h, "/dashboard", &pilot).await.body.contains(uri),
        "sidebar link"
    );

    // A page no [[pages]] rule covers is for admins only.
    let secret = "/plugins/nmu.pages/admin/secret";
    assert_eq!(page(&h, secret, &pilot).await.status, StatusCode::NOT_FOUND);
    assert!(!page(&h, "/dashboard", &pilot).await.body.contains(secret));
    assert_eq!(page(&h, secret, &owner).await.status, StatusCode::OK);
    assert!(page(&h, "/dashboard", &owner).await.body.contains(secret));

    // Posting is checked the same way.
    let res = send(
        &h.app,
        form("/plugins/nmu.pages/admin/secret", "_form=x", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // Paths that aren't link paths never reach the plugin.
    for bad in [
        "/plugins/nmu.pages/bad%20path",
        "/plugins/nmu.pages/a//b",
        "/plugins/Not.An.Id/values",
    ] {
        assert_eq!(
            page(&h, bad, &owner).await.status,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_host_draws_what_the_plugin_describes(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let res = page(&h, "/plugins/nmu.pages/values", &owner).await;
    assert_eq!(res.status, StatusCode::OK);
    for part in [
        "1.24b",
        "1,240,000,000 ISK",
        "2026-09-24 18:00",
        r#"href="/plugins/nmu.pages/moons/old""#,
        "Viewing as Chribba",
        "first tab",
        r#"href="/plugins/nmu.pages/values?_tab=1""#,
    ] {
        assert!(res.body.contains(part), "{part}: {}", res.body);
    }
    assert!(!res.body.contains("second tab"));
    let second = page(&h, "/plugins/nmu.pages/values?_tab=1", &owner).await;
    assert!(second.body.contains("second tab") && !second.body.contains("first tab"));

    // The query reaches the plugin, without the host's own parameters.
    let query = page(&h, "/plugins/nmu.pages/query?moon=1&_tab=0", &owner).await;
    assert!(query.body.contains("moon"), "{}", query.body);
    assert!(!query.body.contains("_tab"), "{}", query.body);
    let long = format!("/plugins/nmu.pages/query?q={}", "x".repeat(3000));
    assert_eq!(
        page(&h, &long, &owner).await.status,
        StatusCode::BAD_REQUEST
    );

    // What went wrong goes to the plugin's log, not to the user.
    assert_eq!(
        page(&h, "/plugins/nmu.pages/missing", &owner).await.status,
        StatusCode::NOT_FOUND
    );
    for path in ["failed", "crash"] {
        let res = page(&h, &format!("/plugins/nmu.pages/{path}"), &owner).await;
        assert_eq!(
            res.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{path}: {}",
            res.body
        );
        assert!(res.body.contains("be shown. The app"), "{path}");
        assert!(!res.body.contains("on fire"), "{path}");
    }
    let logged: Vec<String> = sqlx::query_scalar(
        "SELECT message FROM core.plugin_logs WHERE plugin_id = $1 AND level = 'error'",
    )
    .bind(ID)
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert!(logged.iter().any(|l| l.contains("on fire")), "{logged:?}");
}

fn post(uri: &str, body: &str, token: &str) -> Request<Body> {
    form(uri, body, token)
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn forms_are_checked_before_the_plugin_sees_them(db: PgPool) {
    let (h, owner, pilot) = setup(db).await;
    let uri = "/plugins/nmu.pages/form";
    let shown = page(&h, uri, &owner).await;
    assert!(
        shown.body.contains(r#"name="_form" value="note""#),
        "{}",
        shown.body
    );
    assert!(shown.body.contains(r#"maxlength="20""#));

    let ok = send(
        &h.app,
        post(uri, "_form=note&body=hi&count=3&kind=ore", &owner),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert!(ok.body.contains("got"), "{}", ok.body);
    // One value per field, in order; the checkbox is filled in.
    assert!(ok.body.contains("go"), "{}", ok.body);

    for (body, status) in [
        (
            "_form=note&body=hi&extra=1",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("_form=note&body=", StatusCode::UNPROCESSABLE_ENTITY),
        (
            "_form=note&body=hi&count=11",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&count=2.5",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&kind=gas",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=hi&body=again",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "_form=note&body=this%20is%20far%20more%20than%20twenty%20characters",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        ("body=hi", StatusCode::BAD_REQUEST),
        ("_form=other&body=hi", StatusCode::CONFLICT),
    ] {
        let res = send(&h.app, post(uri, body, &owner)).await;
        assert_eq!(res.status, status, "{body}");
        assert!(!res.body.contains("got ["), "the plugin never saw {body}");
    }

    // Numbers arrive written one way, whatever was typed.
    let res = send(&h.app, post(uri, "_form=note&body=hi&count=%2B3.0", &owner)).await;
    assert!(
        res.body.contains("&#34;count&#34;, &#34;3&#34;"),
        "{}",
        res.body
    );
    // Too big a post is refused before anything reads it.
    let huge = format!("_form=note&body={}", "x".repeat(70 * 1024));
    let res = send(&h.app, post(uri, &huge, &owner)).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);

    // A plugin can send the user on to another of its pages.
    let res = send(&h.app, post(uri, "_form=note&body=hi&go=on", &owner)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/plugins/nmu.pages/values");

    // Other sites can't post (the Origin check), and posting is rate-limited.
    let no_origin = Request::post(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={owner}"))
        .body(Body::from("_form=note&body=hi"))
        .unwrap();
    assert_eq!(send(&h.app, no_origin).await.status, StatusCode::FORBIDDEN);
    grant_view(&h.db).await;
    let mut limited = false;
    for _ in 0..31 {
        let res = send(&h.app, post(uri, "_form=note&body=hi", &pilot)).await;
        if res.status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
        assert_eq!(res.status, StatusCode::OK);
    }
    assert!(limited, "30 a minute per account and plugin");

    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.plugin_logs WHERE plugin_id = $1 AND message = 'submitted note'",
    )
    .bind(ID)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(logged >= 2);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn plugin_permissions_are_granted_like_core_ones(db: PgPool) {
    let (h, owner, _) = setup(db).await;
    let listed = page(&h, "/admin/permissions", &owner).await.body;
    assert!(listed.contains("plugin.nmu.pages.view") && listed.contains("See the pages"));

    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.nmu.pages.view&grantee=state%3A{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=plugin.nmu.pages.nope&grantee=state%3A{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // The owner has every permission, plugins' included.
    let me = me(&h, &owner).await;
    assert!(
        me["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "plugin.nmu.pages.view"),
        "{me}"
    );

    // Uninstalling takes the permission and its grants with it.
    let res = send(
        &h.app,
        form(
            "/admin/plugins/nmu.pages/uninstall",
            "confirmation=nmu.pages",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.permission_grants WHERE permission LIKE 'plugin.%'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert!(
        !page(&h, "/admin/permissions", &owner)
            .await
            .body
            .contains("plugin.nmu.pages")
    );
}
