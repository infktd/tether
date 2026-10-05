//! Staying on the page (DESIGN.md, Page hygiene and state): what a
//! boosted form post's answer becomes, so the viewer keeps their page,
//! tab, query and scroll position, with a toast; and that without
//! JavaScript the plain redirects still work.

use axum::http::StatusCode;
use sqlx::PgPool;

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba";

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_redirect_back_reloads_the_page_in_place_keeping_its_query(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let grant = |permission: &str| format!("permission={permission}&grantee=state:1");
    let here = "/admin/permissions?q=states";

    // Without JavaScript: the redirect it always was.
    let plain = send(
        &h.app,
        form("/admin/permissions/set", &grant("admin.states"), &owner),
    )
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    assert_eq!(plain.location(), "/admin/permissions");

    // From the page, boosted: the same page, its query kept, in place, no
    // new history entry.
    let res = send(
        &h.app,
        boosted(
            form("/admin/permissions/set", &grant("admin.groups"), &owner),
            here,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = hx_location(&res).expect("HX-Location");
    assert_eq!(to["path"], here);
    assert_eq!(to["target"], "body");
    assert_eq!(to["swap"], "innerHTML show:none");
    assert_eq!(to["push"], "false");
    let (message, tone) = toast(&res).expect("a toast");
    assert!(message.contains("granted to Member"), "{message}");
    assert_eq!(tone, "done");

    // Posted from another page (or a forged address elsewhere): where the
    // handler said, as a navigation.
    for (current, permission) in [
        ("/admin", "admin.users"),
        ("https://evil.test/admin/permissions?q=x", "admin.audit"),
    ] {
        let mut req = boosted(
            form("/admin/permissions/set", &grant(permission), &owner),
            "/",
        );
        req.headers_mut().insert(
            "hx-current-url",
            if current.starts_with("https://") {
                current.to_owned()
            } else {
                format!("{SITE}{current}")
            }
            .parse()
            .unwrap(),
        );
        let res = send(&h.app, req).await;
        let to = hx_location(&res).expect("HX-Location");
        assert_eq!(to["path"], "/admin/permissions", "{current}");
        assert_eq!(to["swap"], "innerHTML show:top", "{current}");
    }
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn an_error_page_becomes_a_toast_and_the_page_stays(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let res = send(
        &h.app,
        boosted(form("/no/such/action", "", &owner), "/groups"),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
    assert!(res.body.is_empty());
    assert_eq!(
        toast(&res),
        Some((
            "There's nothing at this address.".to_owned(),
            "problem".to_owned()
        ))
    );
    // Without JavaScript, the error page.
    let plain = send(&h.app, form("/no/such/action", "", &owner)).await;
    assert_eq!(plain.status, StatusCode::NOT_FOUND);
    assert!(plain.body.contains("nothing at this address"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_page_answering_a_post_is_swapped_in_place(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // A refused form: the page again with the reason, at the page's own
    // address (not the post's), keeping the scroll position.
    let res = send(
        &h.app,
        boosted(form("/admin/states", "name=", &owner), "/admin/states"),
    )
    .await;
    assert!(res.status.is_client_error(), "{}", res.status);
    assert!(res.body.contains("<html"), "{}", res.body);
    assert_eq!(res.headers["hx-reswap"], "innerHTML show:none");
    assert_eq!(res.headers["hx-push-url"], "false");
}

/// Editing in place (DESIGN.md): a form in a `data-in-place` region names
/// it in `HX-In-Place`; back to the very page it was on, only that region
/// (and what it names besides) is taken from the page and swapped.
#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_row_edited_in_place_swaps_only_that_row(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let here = "/admin/permissions?q=states";
    let row = "perm-admin_2e_states";
    let shown = page(&h, here, &owner).await;
    assert!(
        shown
            .body
            .contains(&format!(r#"<tr id="{row}" data-in-place>"#)),
        "{}",
        shown.body
    );

    let post = |grantee: &str, regions: &str| {
        let mut req = boosted(
            form(
                "/admin/permissions/set",
                &format!("permission=admin.states&grantee={grantee}"),
                &owner,
            ),
            here,
        );
        req.headers_mut()
            .insert("hx-in-place", regions.parse().unwrap());
        req
    };
    let res = send(&h.app, post("state:1", &format!("#{row}"))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    let to = hx_location(&res).expect("HX-Location");
    assert_eq!(to["path"], here);
    assert_eq!(to["target"], format!("#{row}"));
    assert_eq!(to["select"], format!("#{row}"));
    assert_eq!(to["swap"], "outerHTML show:none");
    assert_eq!(to["push"], "false");
    assert!(to.get("selectOOB").is_none(), "{to}");
    assert!(toast(&res).is_some());
    // The page it's taken from has the row, now granted.
    let after = page(&h, here, &owner).await;
    let at = after.body.find(&format!(r#"id="{row}""#)).unwrap();
    assert!(after.body[at..].contains("State: Member"), "{}", after.body);

    // A name that isn't plain: the whole page, as ever.
    let res = send(&h.app, post("state:1", "#a [onclick]")).await;
    let to = hx_location(&res).expect("HX-Location");
    assert_eq!(to["target"], "body");
    assert!(to.get("select").is_none(), "{to}");

    // States: covering someone swaps the covers and every count.
    let states = page(&h, "/admin/states", &owner).await;
    assert!(
        states.body.contains(r##"data-in-place="#covers-"##),
        "{}",
        states.body
    );
    assert!(
        states
            .body
            .contains(r#"class="state-accounts" id="accounts-"#)
    );
}
