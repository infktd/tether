//! Menu customization (AA's Menu): admins arrange the sidebar; everyone
//! still sees only what they may open.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";
const GIGX: &str = "1887431749:gigX";

async fn edit(h: &Harness, token: &str, action: &str, body: &str) -> Res {
    send(&h.app, form(&format!("/admin/menu/{action}"), body, token)).await
}

/// The sidebar's `<nav>`, so page content doesn't count.
fn sidebar(body: &str) -> &str {
    let start = body.find(r#"aria-label="Main""#).unwrap();
    let end = body[start..].find("</nav>").unwrap() + start;
    &body[start..end]
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_arrange_the_sidebar(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    assert_eq!(
        page(&h, "/admin/menu", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    let before = page(&h, "/dashboard", &owner).await.body;
    let nav = sidebar(&before);
    assert!(nav.contains(">Account<") && nav.contains(r#"href="/admin/system""#));

    // A section with a folder for the admin pages, and a custom link.
    let res = edit(&h, &owner, "sections", "label=Leadership").await;
    assert_eq!(res.location(), "/admin/menu", "{}", res.body);
    let section: i64 = sqlx::query_scalar(
        "SELECT id FROM core.menu_entries WHERE kind = 'section' AND label = 'Leadership'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    let res = edit(
        &h,
        &owner,
        "folders",
        &format!("label=Access&parent=id%3A{section}"),
    )
    .await;
    assert_eq!(res.location(), "/admin/menu", "{}", res.body);
    let folder: i64 = sqlx::query_scalar("SELECT id FROM core.menu_entries WHERE kind = 'folder'")
        .fetch_one(&h.db)
        .await
        .unwrap();
    for item in ["states", "permissions"] {
        let res = edit(
            &h,
            &owner,
            "change",
            &format!("reference={item}&label=&parent=id%3A{folder}"),
        )
        .await;
        assert_eq!(res.location(), "/admin/menu", "{item}: {}", res.body);
    }
    let res = edit(
        &h,
        &owner,
        "links",
        &format!(
            "label=Wiki&url=https%3A%2F%2Fwiki.example%2Fnmu&new_tab=on&parent=id%3A{section}"
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/menu", "{}", res.body);
    // Renamed and hidden.
    edit(
        &h,
        &owner,
        "change",
        "reference=tokens&label=My+Tokens&parent=section%3Aaccount",
    )
    .await;
    edit(
        &h,
        &owner,
        "change",
        "reference=services&label=&parent=section%3Aaccount&hidden=on",
    )
    .await;

    let after = page(&h, "/dashboard", &owner).await.body;
    let nav = sidebar(&after);
    assert!(nav.contains(">Leadership<"), "{nav}");
    assert!(nav.contains("<details class=\"nav-folder\""), "{nav}");
    assert!(nav.contains(r#"href="https://wiki.example/nmu" class="nav-item" target="_blank" rel="noopener noreferrer""#), "{nav}");
    assert!(nav.contains("My Tokens"), "{nav}");
    assert!(!nav.contains(r#"href="/services""#), "hidden: {nav}");
    // The folder opens on its own page.
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(
        sidebar(&states).contains("nav-folder\" open"),
        "{}",
        sidebar(&states)
    );

    // Moving: Leadership goes above Account.
    for _ in 0..5 {
        edit(
            &h,
            &owner,
            "move",
            &format!("reference=id%3A{section}&dir=up"),
        )
        .await;
    }
    let moved = page(&h, "/dashboard", &owner).await.body;
    let nav = sidebar(&moved);
    assert!(
        nav.find(">Leadership<").unwrap() < nav.find(">Account<").unwrap(),
        "{nav}"
    );

    // The pilot sees the link and their own pages, never admin pages.
    let theirs = page(&h, "/dashboard", &pilot).await.body;
    let nav = sidebar(&theirs);
    assert!(nav.contains("Wiki"), "{nav}");
    assert!(!nav.contains("/admin/states"), "{nav}");
    assert!(
        !nav.contains(">Access<"),
        "an empty folder doesn't show: {nav}"
    );

    // Only what was added can be deleted; a default section can't.
    let res = edit(&h, &owner, "delete", "reference=section%3Aadmin").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = edit(&h, &owner, "delete", &format!("reference=id%3A{section}")).await;
    assert_eq!(res.location(), "/admin/menu");
    let back = page(&h, "/admin/states", &owner).await.body;
    let nav = sidebar(&back);
    assert!(
        !nav.contains("Leadership") && !nav.contains("Wiki"),
        "{nav}"
    );
    assert!(
        nav.contains(r#"href="/admin/states""#),
        "back in Admin: {nav}"
    );

    let res = edit(&h, &owner, "reset", "").await;
    assert_eq!(res.location(), "/admin/menu");
    let reset = page(&h, "/dashboard", &owner).await.body;
    assert!(sidebar(&reset).contains(r#"href="/services""#));
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action LIKE 'menu.%'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(audited >= 8, "{audited}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn links_and_names_are_checked(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    for (body, why) in [
        (
            "label=X&url=javascript%3Aalert(1)&parent=section%3Aaccount",
            "https://",
        ),
        (
            "label=X&url=%2F%2Fevil.example&parent=section%3Aaccount",
            "https://",
        ),
        (
            "label=X&url=http%3A%2F%2Fexample.com&parent=section%3Aaccount",
            "https://",
        ),
        (
            "label=&url=%2Fgroups&parent=section%3Aaccount",
            "Give it a name",
        ),
        (
            "label=X&url=%2Fgroups&parent=dashboard",
            "section or folder",
        ),
    ] {
        let res = edit(&h, &owner, "links", body).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(res.body.contains(why), "{body}: {}", res.body);
    }
    // A page here, even the front page, but never another site.
    let res = edit(
        &h,
        &owner,
        "links",
        "label=Home&url=%2F&parent=section%3Aaccount",
    )
    .await;
    assert_eq!(res.location(), "/admin/menu", "{}", res.body);
    let res = edit(
        &h,
        &owner,
        "links",
        "label=X&url=%2F%5Cevil.example&parent=section%3Aaccount",
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = edit(&h, &owner, "sections", "label=Sneaky%E2%80%AEname").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let long = "x".repeat(41);
    let res = edit(&h, &owner, "sections", &format!("label={long}")).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = edit(
        &h,
        &owner,
        "change",
        "reference=nothing&parent=section%3Aaccount",
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
