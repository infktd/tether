#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use sqlx::PgPool;

fn form(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::ORIGIN, SITE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("{SESSION}={token}"))
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn page(h: &Harness, uri: &str, token: &str) -> Res {
    send(&h.app, get(uri, &[(SESSION, token)])).await
}

async fn owner_and_pilot(h: &Harness) -> (String, String) {
    let owner = log_in_owner(h, "196379789:Chribba").await;
    let pilot = log_in_as(h, "443630591:The Mittani", None).await;
    (owner, pilot)
}

fn assert_no_external_urls(html: &str) {
    for (i, _) in html.match_indices("http") {
        let rest = &html[i..];
        if rest.starts_with("https://") || rest.starts_with("http://") {
            assert!(
                rest.starts_with("https://images.evetech.net/"),
                "external URL: {}",
                &rest[..60.min(rest.len())]
            );
        }
    }
}

async fn create_group(h: &Harness, owner: &str, name: &str, policy: &str) -> String {
    let res = send(
        &h.app,
        form(
            "/admin/groups",
            &format!("name={name}&description=&join_policy={policy}"),
            owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.location().to_owned()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admin_pages_need_a_session_and_the_permission(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;

    for uri in [
        "/admin/groups",
        "/admin/permissions",
        "/admin/states",
        "/admin",
    ] {
        assert_eq!(
            send(&h.app, get(uri, &[])).await.location(),
            "/login",
            "{uri}"
        );
        assert_eq!(
            page(&h, uri, &pilot).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        let res = page(&h, uri, &owner).await;
        assert!(
            res.status == StatusCode::OK || res.status == StatusCode::SEE_OTHER,
            "{uri}: {}",
            res.status
        );
    }
    // Forms refuse too.
    let res = send(
        &h.app,
        form("/admin/groups", "name=X&join_policy=open", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_sidebar_shows_only_permitted_admin_links(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;

    let owner_nav = page(&h, "/dashboard", &owner).await.body;
    for link in [
        "/admin/groups",
        "/admin/permissions",
        "/admin/states",
        "/setup",
    ] {
        assert!(
            owner_nav.contains(&format!(r#"href="{link}""#)),
            "owner sees {link}"
        );
    }
    let pilot_nav = page(&h, "/dashboard", &pilot).await.body;
    assert!(
        !pilot_nav.contains("Admin</div>"),
        "no Admin section for a plain pilot"
    );

    // Grant states to an assigned group the pilot is in: only that
    // link appears.
    let group = create_group(&h, &owner, "State Wranglers", "assigned").await;
    let group_id = group.rsplit('/').next().unwrap().to_owned();
    send(
        &h.app,
        form(&format!("{group}/members"), "character=the+mittani", &owner),
    )
    .await;
    send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.states&grantee=group:{group_id}"),
            &owner,
        ),
    )
    .await;
    let pilot_nav = page(&h, "/dashboard", &pilot).await.body;
    assert!(pilot_nav.contains(r#"href="/admin/states""#));
    assert!(!pilot_nav.contains(r#"href="/admin/groups""#));
    assert_eq!(
        page(&h, "/admin/states", &pilot).await.status,
        StatusCode::OK
    );
    assert_eq!(
        page(&h, "/admin/groups", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn groups_are_created_filled_and_deleted_from_the_pages(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;

    let group = create_group(&h, &owner, "Officers", "assigned").await;
    let list = page(&h, "/admin/groups", &owner).await;
    assert!(list.body.contains("Officers"));
    assert!(list.body.contains("Assigned by admins"));
    assert_no_external_urls(&list.body);

    let dup = send(
        &h.app,
        form("/admin/groups", "name=Officers&join_policy=open", &owner),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert!(dup.body.contains("already exists"));

    let added = send(
        &h.app,
        form(&format!("{group}/members"), "character=the+mittani", &owner),
    )
    .await;
    assert_eq!(added.location(), group);
    let detail = page(&h, &group, &owner).await;
    assert!(detail.body.contains("The Mittani"));
    assert!(
        detail
            .body
            .contains("https://images.evetech.net/characters/443630591/portrait")
    );
    assert_eq!(
        me(&h, &pilot).await["groups"],
        serde_json::json!(["Officers"])
    );

    let unknown = send(
        &h.app,
        form(&format!("{group}/members"), "character=nobody", &owner),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert!(
        unknown
            .body
            .contains("No account has a character with that name")
    );

    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let removed = send(
        &h.app,
        form(
            &format!("{group}/members/{pilot_account}/remove"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(removed.status, StatusCode::SEE_OTHER);
    assert_eq!(me(&h, &pilot).await["groups"], serde_json::json!([]));

    let deleted = send(&h.app, form(&format!("{group}/delete"), "", &owner)).await;
    assert_eq!(deleted.location(), "/admin/groups");
    assert!(
        !page(&h, "/admin/groups", &owner)
            .await
            .body
            .contains("Officers")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn requests_are_approved_and_denied_from_the_group_page(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let other = log_in_as(&h, "1887431749:gigX", None).await;
    let group = create_group(&h, &owner, "Capitals", "request").await;
    let id = group.rsplit('/').next().unwrap();
    for token in [&pilot, &other] {
        send(
            &h.app,
            post_json(&format!("/api/groups/{id}/join"), token, ""),
        )
        .await;
    }
    let detail = page(&h, &group, &owner).await;
    assert!(detail.body.contains("Requests to join"));

    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let other_account = me(&h, &other).await["account_id"].as_i64().unwrap();
    send(
        &h.app,
        form(
            &format!("{group}/requests/{pilot_account}/approve"),
            "",
            &owner,
        ),
    )
    .await;
    send(
        &h.app,
        form(
            &format!("{group}/requests/{other_account}/deny"),
            "",
            &owner,
        ),
    )
    .await;

    assert_eq!(
        me(&h, &pilot).await["groups"],
        serde_json::json!(["Capitals"])
    );
    assert_eq!(me(&h, &other).await["groups"], serde_json::json!([]));
    assert!(
        !page(&h, &group, &owner)
            .await
            .body
            .contains("Requests to join")
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn permissions_are_granted_and_revoked_from_the_page(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let group = create_group(&h, &owner, "Officers", "assigned").await;
    let group_id = group.rsplit('/').next().unwrap().to_owned();
    send(
        &h.app,
        form(&format!("{group}/members"), "character=the+mittani", &owner),
    )
    .await;

    send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.audit&grantee=state:{MEMBER_STATE}"),
            &owner,
        ),
    )
    .await;
    send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.groups&grantee=group:{group_id}"),
            &owner,
        ),
    )
    .await;
    let listed = page(&h, "/admin/permissions", &owner).await;
    assert!(listed.body.contains("State: Member"));
    assert!(listed.body.contains("Group: Officers"));
    assert_eq!(
        me(&h, &pilot).await["permissions"],
        serde_json::json!(["admin.groups"])
    );
    assert_no_external_urls(&listed.body);

    let bad = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            "permission=admin.audit&grantee=everyone",
            &owner,
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let grant_id: i64 =
        sqlx::query_scalar("SELECT id FROM core.permission_grants WHERE group_id IS NOT NULL")
            .fetch_one(&h.db)
            .await
            .unwrap();
    let revoked = send(
        &h.app,
        form(&format!("/admin/permissions/{grant_id}/revoke"), "", &owner),
    )
    .await;
    assert_eq!(revoked.location(), "/admin/permissions");
    assert_eq!(me(&h, &pilot).await["permissions"], serde_json::json!([]));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admin_permissions_never_go_to_guest_or_open_groups(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let open = create_group(&h, &owner, "Anyone", "open").await;
    let open_id = open.rsplit('/').next().unwrap().to_owned();

    let guest = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.audit&grantee=state:{GUEST_STATE}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(guest.status, StatusCode::BAD_REQUEST);
    assert!(guest.body.contains("anyone who logs in with EVE is Guest"));

    let open_grant = send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.permissions&grantee=group:{open_id}"),
            &owner,
        ),
    )
    .await;
    assert_eq!(open_grant.status, StatusCode::BAD_REQUEST);
    assert!(open_grant.body.contains("anyone can join it"));

    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM core.permission_grants")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(grants, 0);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn group_managers_cannot_add_anyone_to_a_group_with_more_power_than_theirs(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, pilot) = owner_and_pilot(&h).await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    // The pilot manages groups...
    let officers = create_group(&h, &owner, "Officers", "assigned").await;
    let officers_id = officers.rsplit('/').next().unwrap().to_owned();
    send(
        &h.app,
        form(
            &format!("{officers}/members"),
            "character=the+mittani",
            &owner,
        ),
    )
    .await;
    send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.groups&grantee=group:{officers_id}"),
            &owner,
        ),
    )
    .await;
    // ...but a group grants admin.permissions, which the pilot doesn't have.
    let admins = create_group(&h, &owner, "Admins", "assigned").await;
    let admins_id = admins.rsplit('/').next().unwrap().to_owned();
    send(
        &h.app,
        form(
            "/admin/permissions/grant",
            &format!("permission=admin.permissions&grantee=group:{admins_id}"),
            &owner,
        ),
    )
    .await;
    let plain = create_group(&h, &owner, "Miners", "assigned").await;

    let escalate = send(
        &h.app,
        form(
            &format!("{admins}/members"),
            "character=the+mittani",
            &pilot,
        ),
    )
    .await;
    assert_eq!(escalate.status, StatusCode::FORBIDDEN);
    assert!(escalate.body.contains("admin.permissions"));
    let via_api = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{admins_id}/members"),
            &pilot,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert_eq!(via_api.status, StatusCode::FORBIDDEN);
    assert_eq!(
        me(&h, &pilot).await["permissions"],
        serde_json::json!(["admin.groups"])
    );

    // Groups within the pilot's own power are fine.
    let ok = send(
        &h.app,
        form(&format!("{plain}/members"), "character=the+mittani", &pilot),
    )
    .await;
    assert_eq!(ok.status, StatusCode::SEE_OTHER);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn group_descriptions_are_capped(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;
    let long = "x".repeat(501);
    let res = send(
        &h.app,
        form(
            "/admin/groups",
            &format!("name=Big&description={long}&join_policy=open"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("at most 500 characters"));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn states_are_found_previewed_added_and_removed_from_the_page(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;

    let listed = page(&h, "/admin/states", &owner).await;
    assert_eq!(listed.status, StatusCode::OK);
    for name in ["Member", "Blue", "Guest"] {
        assert!(listed.body.contains(name), "{name}");
    }
    assert!(listed.body.contains("Guest lists nobody"));
    assert_no_external_urls(&listed.body);

    let mut search = form(
        "/admin/states/search",
        &format!("state_id={BLUE_STATE}&name=Goonswarm+Federation"),
        &owner,
    );
    search
        .headers_mut()
        .insert("hx-request", "true".parse().unwrap());
    let found = send(&h.app, search).await;
    assert!(found.body.contains("Goonswarm Federation"));
    assert!(found.body.contains("Add to Blue"));
    assert!(found.body.contains("https://images.evetech.net/alliances/"));
    assert!(!found.body.contains("<html"));

    // The owner's main (Chribba) is in Otherworld Empire: adding it to Blue
    // moves them, so the page asks first and changes nothing yet.
    let add = format!("/admin/states/{BLUE_STATE}/covers");
    let asked = send(&h.app, form(&add, "entity_id=159826257", &owner)).await;
    assert_eq!(asked.status, StatusCode::OK);
    assert!(asked.body.contains("Add Otherworld Empire to Blue?"));
    assert!(asked.body.contains(r#"name="confirm" value="1""#));
    assert!(
        !page(&h, "/admin/states", &owner)
            .await
            .body
            .contains("Otherworld Empire")
    );

    let added = send(&h.app, form(&add, "entity_id=159826257&confirm=1", &owner)).await;
    assert_eq!(added.location(), "/admin/states");
    let listed = page(&h, "/admin/states", &owner).await;
    assert!(listed.body.contains("Otherworld Empire"));
    assert!(
        listed
            .body
            .contains("https://images.evetech.net/alliances/159826257/logo")
    );

    // Guest lists nobody.
    let guest = send(
        &h.app,
        form(
            &format!("/admin/states/{GUEST_STATE}/covers"),
            "entity_id=159826257&confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(guest.status, StatusCode::BAD_REQUEST);

    let removed = send(
        &h.app,
        form(
            &format!("/admin/states/{BLUE_STATE}/covers/159826257/remove"),
            "confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(removed.location(), "/admin/states");
    assert!(
        !page(&h, "/admin/states", &owner)
            .await
            .body
            .contains("Otherworld Empire")
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM core.audit_log WHERE action LIKE 'state.%' AND action <> 'state.change' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(actions, ["state.add", "state.remove"]);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn states_are_created_ordered_renamed_and_deleted(db: PgPool) {
    let h = harness(db, true).await;
    let (owner, _) = owner_and_pilot(&h).await;

    let created = send(&h.app, form("/admin/states", "name=Trial", &owner)).await;
    assert_eq!(created.location(), "/admin/states");
    let order = |body: &str| {
        ["Member", "Blue", "Trial", "Guest"].map(|n| body.find(&format!(">{n}</span>")).unwrap())
    };
    let o = order(&page(&h, "/admin/states", &owner).await.body);
    assert!(
        o[0] < o[1] && o[1] < o[2] && o[2] < o[3],
        "new states go just above Guest"
    );

    let taken = send(&h.app, form("/admin/states", "name=trial", &owner)).await;
    assert_eq!(taken.status, StatusCode::CONFLICT);

    let trial: i64 = sqlx::query_scalar("SELECT id FROM core.states WHERE name = 'Trial'")
        .fetch_one(&h.db)
        .await
        .unwrap();
    // Nobody moves (Trial covers nobody), so no confirmation is needed.
    let up = send(
        &h.app,
        form(
            &format!("/admin/states/{trial}/move"),
            "direction=up",
            &owner,
        ),
    )
    .await;
    assert_eq!(up.location(), "/admin/states");
    let body = page(&h, "/admin/states", &owner).await.body;
    assert!(body.find(">Trial</span>").unwrap() < body.find(">Blue</span>").unwrap());
    // Nothing moves below Guest, and Guest doesn't move.
    let guest = send(
        &h.app,
        form(
            &format!("/admin/states/{GUEST_STATE}/move"),
            "direction=up",
            &owner,
        ),
    )
    .await;
    assert_eq!(guest.status, StatusCode::BAD_REQUEST);

    let renamed = send(
        &h.app,
        form(
            &format!("/admin/states/{trial}/rename"),
            "name=Recruits",
            &owner,
        ),
    )
    .await;
    assert_eq!(renamed.location(), "/admin/states");
    let builtin = send(
        &h.app,
        form(
            &format!("/admin/states/{MEMBER_STATE}/rename"),
            "name=Pilots",
            &owner,
        ),
    )
    .await;
    assert_eq!(builtin.status, StatusCode::BAD_REQUEST);
    let builtin = send(
        &h.app,
        form(
            &format!("/admin/states/{MEMBER_STATE}/delete"),
            "confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(builtin.status, StatusCode::BAD_REQUEST);

    let deleted = send(
        &h.app,
        form(&format!("/admin/states/{trial}/delete"), "", &owner),
    )
    .await;
    assert_eq!(deleted.location(), "/admin/states");
    assert!(
        !page(&h, "/admin/states", &owner)
            .await
            .body
            .contains("Recruits")
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM core.audit_log WHERE action LIKE 'state.%' AND action <> 'state.change' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        ["state.create", "state.move", "state.rename", "state.delete"]
    );
}
