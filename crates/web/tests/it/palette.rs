//! The command palette: pages, pilots and actions, only what the viewer
//! may open, filtered as the sidebar, Administration and the apps' frames
//! are.

use std::sync::OnceLock;

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::PgPool;
use tether_core::states::StateId;
use tether_db::accounts::AccountId;
use tether_db::permissions::Grantee;
use tether_plugins::testing::{self, Key};

/// The palette's own search: an htmx request for its results.
async fn search(h: &Harness, token: &str, query: &str) -> Res {
    let req = Request::get(format!("/palette?{query}"))
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    send(&h.app, req).await
}

async fn account_of(h: &Harness, character: i64) -> i64 {
    sqlx::query_scalar("SELECT account_id FROM core.characters WHERE id = $1")
        .bind(character)
        .fetch_one(&h.db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_pilot_sees_only_what_they_may_open(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner Pilot").await;
    let pilot = log_in_as(&h, "90000002:Line Pilot", None).await;

    let theirs = search(&h, &pilot, "").await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.body);
    let body = &theirs.body;
    // A fragment: the results, no page around them, never kept.
    assert!(!body.contains("<html"), "{body}");
    assert_eq!(theirs.headers["cache-control"], "no-store");
    assert!(
        theirs.headers["vary"]
            .to_str()
            .unwrap()
            .contains("HX-Request")
    );
    assert!(body.contains(r#"role="listbox""#), "{body}");
    // Pages anyone signed in may open, and the account's own actions.
    assert!(body.contains(r#"href="/dashboard""#), "{body}");
    assert!(body.contains("Log out"), "{body}");
    assert!(body.contains(r#"action="/register/start""#), "{body}");
    // Nothing of Administration's, no pilots, no admin actions.
    for hidden in [
        "/admin/states",
        "/admin/users",
        "/admin/system",
        "/admin/groups",
        "New group",
        "New state",
    ] {
        assert!(!body.contains(hidden), "{hidden} in {body}");
    }
    let pages = search(&h, &pilot, "q=states&scope=pages").await.body;
    assert!(!pages.contains("/admin/states"), "{pages}");
    assert!(pages.contains("Nothing you may open matches"), "{pages}");

    // The owner may open everything: every Administration page (pinned in
    // the sidebar or not), and its actions.
    let all = search(&h, &owner, "scope=pages").await.body;
    for shown in [
        "/admin/states",
        "/admin/users",
        "/admin/system",
        "/admin/menu",
        "/admin/audit",
    ] {
        assert!(all.contains(shown), "{shown} missing from {all}");
    }
    assert!(all.contains("Administration · Access"), "{all}");
    let actions = search(&h, &owner, "scope=actions").await.body;
    assert!(actions.contains("/admin/groups#new-group"), "{actions}");
    assert!(actions.contains("/admin/states#new-state"), "{actions}");
    // Only actions in the Actions scope.
    assert!(!actions.contains(r#"href="/dashboard""#), "{actions}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_permission_granted_shows_its_pages_and_actions(db: PgPool) {
    let h = harness(db, true).await;
    let _owner = log_in_owner(&h, "90000001:Owner Pilot").await;
    let pilot = log_in_as(&h, "90000002:Line Pilot", None).await;
    let account = AccountId(account_of(&h, 90000002).await);
    tether_db::permissions::grant(
        &h.db,
        tether_core::permissions::ADMIN_GROUPS,
        Grantee::Account(account),
    )
    .await
    .unwrap();
    let body = search(&h, &pilot, "q=group").await.body;
    assert!(body.contains(r#"href="/admin/groups""#), "{body}");
    assert!(body.contains("/admin/groups#new-group"), "{body}");
    // Only that: States needs its own permission.
    assert!(!body.contains("/admin/states"), "{body}");
    assert!(
        !search(&h, &pilot, "q=state")
            .await
            .body
            .contains("New state")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn pilots_only_for_those_who_may_open_users(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner Pilot").await;
    let pilot = log_in_as(&h, "90000002:Line Pilot", None).await;
    let owner_account = account_of(&h, 90000001).await;
    let found = format!(r#"href="/admin/users/{owner_account}""#);

    // A pilot without Users finds no pilots, even asking for them.
    let theirs = search(&h, &pilot, "q=owner&scope=pilots").await.body;
    assert!(!theirs.contains(&found), "{theirs}");
    assert!(!theirs.contains(">Pilots<"), "{theirs}");

    // The owner may open Users: the account, by any character's name.
    let ours = search(&h, &owner, "q=owner").await.body;
    assert!(ours.contains(&found), "{ours}");
    assert!(ours.contains(">Pilots<"), "{ours}");
    // One letter isn't a search for pilots yet.
    let short = search(&h, &owner, "q=o&scope=pilots").await.body;
    assert!(!short.contains(&found), "{short}");
    assert!(short.contains("two letters"), "{short}");

    // Granted Users, the pilot finds them too.
    tether_db::permissions::grant(
        &h.db,
        tether_core::permissions::ADMIN_USERS,
        Grantee::Account(AccountId(account_of(&h, 90000002).await)),
    )
    .await
    .unwrap();
    let now = search(&h, &pilot, "q=owner&scope=pilots").await.body;
    assert!(now.contains(&found), "{now}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_palette_is_a_page_without_script(db: PgPool) {
    let h = harness(db, true).await;
    let pilot = log_in_as(&h, "90000002:Line Pilot", None).await;
    let res = page(&h, "/palette?q=token", &pilot).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // The whole page, with its search and the results as plain links.
    assert!(res.body.contains("<html"), "{}", res.body);
    assert!(res.body.contains(r#"href="/tokens""#), "{}", res.body);
    assert!(!res.body.contains(r#"role="option""#), "{}", res.body);
    // Every page's command bar links here, and carries the palette.
    let home = page(&h, "/groups", &pilot).await.body;
    assert!(home.contains(r#"href="/palette""#), "{home}");
    assert!(home.contains(r#"id="palette""#), "{home}");
    // Signed out: to the login, never results.
    let anon = send(&h.app, get("/palette?q=admin", &[])).await;
    assert_eq!(anon.status, StatusCode::SEE_OTHER);
    let anon = send(
        &h.app,
        Request::get("/palette?q=admin")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(!anon.body.contains("/admin"), "{}", anon.body);
}

const APP: &str = "acme.palette";

fn component() -> Vec<u8> {
    static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
    COMPONENT
        .get_or_init(|| build_guest("tether-plugins-test-guest-pages"))
        .clone()
}

/// An app's views, Manage pages and action show to whoever may open them,
/// as its frame shows them.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn app_views_and_actions_follow_their_permissions(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, "90000001:Owner Pilot").await;
    let pilot = log_in_as(&h, "90000002:Line Pilot", None).await;
    let key = Key::new(1);
    let manifest = format!(
        "[plugin]\nid = \"{APP}\"\nname = \"Palette Probe\"\nversion = \"1.0.0\"\nhost_api = \"1\"\n\n\
         [publisher]\nkey = \"{}\"\n\n[permissions]\nview = \"See it\"\nlog = \"Log things\"\n\n\
         [[views]]\nlabel = \"Overview\"\npath = \"\"\n\n\
         [[views]]\nlabel = \"Ledger\"\npath = \"values\"\n\n\
         [action]\nlabel = \"Log a thing\"\npath = \"form\"\n\n\
         [[manage]]\nlabel = \"Reports\"\npath = \"admin/reports\"\n\n\
         [[pages]]\npath = \"\"\npermission = \"view\"\n\n\
         [[pages]]\npath = \"form\"\npermission = \"log\"\n\n\
         [[pages]]\npath = \"admin\"\npermission = \"view\"\n\n\
         [[pages]]\npath = \"admin/reports\"\npermission = \"log\"\n",
        key.public()
    );
    let component = component();
    let bytes = testing::zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("plugin.wasm", &component),
    ]);
    install_package(&h, &owner, &bytes, &key.sign(&bytes)).await;

    // Nothing of it before its permission.
    let none = search(&h, &pilot, "q=palette%20probe").await.body;
    assert!(!none.contains(APP), "{none}");

    for state in [MEMBER_STATE, BLUE_STATE, GUEST_STATE] {
        tether_db::permissions::grant(
            &h.db,
            &format!("plugin.{APP}.view"),
            Grantee::State(StateId(state)),
        )
        .await
        .unwrap();
    }
    let viewing = search(&h, &pilot, "q=palette%20probe").await.body;
    assert!(
        viewing.contains(&format!(r#"href="/plugins/{APP}""#)),
        "{viewing}"
    );
    assert!(
        viewing.contains(&format!(r#"href="/plugins/{APP}/values""#)),
        "{viewing}"
    );
    // The action and the Manage page need `log`.
    assert!(
        !viewing.contains(&format!("/plugins/{APP}/form")),
        "{viewing}"
    );
    assert!(
        !viewing.contains(&format!("/plugins/{APP}/admin/reports")),
        "{viewing}"
    );

    tether_db::permissions::grant(
        &h.db,
        &format!("plugin.{APP}.log"),
        Grantee::Account(AccountId(account_of(&h, 90000002).await)),
    )
    .await
    .unwrap();
    let logging = search(&h, &pilot, "q=palette%20probe").await.body;
    assert!(
        logging.contains(&format!(r#"href="/plugins/{APP}/form""#)),
        "{logging}"
    );
    assert!(logging.contains("Log a thing"), "{logging}");
    assert!(
        logging.contains(&format!(r#"href="/plugins/{APP}/admin/reports""#)),
        "{logging}"
    );
    assert!(logging.contains("Palette Probe · Manage"), "{logging}");
}
