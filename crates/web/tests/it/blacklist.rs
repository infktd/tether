//! The Blacklist and Pilot Log (AA's blacklist app): blacklisted mains'
//! accounts go to the Blacklist state and hold nothing.

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456
const MITTANI: &str = "443630591:The Mittani";

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

async fn evaluate(h: &Harness, account: i64) {
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(account))
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_blacklisted_account_holds_nothing(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    assert_eq!(state_of(&h, &pilot).await, "Member");
    // In a group, holding Member's grants.
    let group = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Miners"}"#),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{}/members", group["id"]),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
    assert!(
        !me(&h, &pilot).await["permissions"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Never the owner, never an NPC corporation.
    let res = send(
        &h.app,
        form("/blacklist", "who=1164409536&reason=test", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.body.contains("owner"), "{}", res.body);
    let res = send(
        &h.app,
        form("/blacklist", "who=1000167&reason=test", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Awoxed+a+Rorqual", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.jobs WHERE kind = 'states.evaluate_all' AND state = 'queued'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(queued >= 1, "everyone is re-evaluated at once");
    evaluate(&h, pilot_account).await;
    assert_eq!(state_of(&h, &pilot).await, "Blacklist");
    let pilot_me = me(&h, &pilot).await;
    assert!(
        pilot_me["permissions"].as_array().unwrap().is_empty(),
        "{pilot_me}"
    );
    assert!(
        pilot_me["groups"].as_array().unwrap().is_empty(),
        "left every group: {pilot_me}"
    );
    let listed = page(&h, "/blacklist", &owner).await.body;
    assert!(listed.contains("CircleOfTwo Holding") && listed.contains("Awoxed a Rorqual"));

    // Admins can't reach the Blacklist state any other way.
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(!states.contains(">Blacklist<"), "not on the States page");
    let blacklist_state: i64 =
        sqlx::query_scalar("SELECT id FROM core.states WHERE builtin = 'blacklist'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"fleet.ping","state_id":{blacklist_state}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    // The audit shows them holding nothing.
    let holders = tether_db::permissions_audit::holders(&h.db, "discord.access_discord")
        .await
        .unwrap();
    assert!(holders.iter().all(|h| h.account_id != pilot_account));

    // Off the list, back to Member.
    let res = send(&h.app, form("/blacklist/98133756/remove", "", &owner)).await;
    assert_eq!(res.location(), "/blacklist");
    evaluate(&h, pilot_account).await;
    assert_eq!(state_of(&h, &pilot).await, "Member");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_pilot_log_keeps_notes(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let officer = log_in_as(&h, MITTANI, None).await;
    assert_eq!(
        page(&h, "/blacklist", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    // The officer may read and add notes, not blacklist.
    let group = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Recruiters"}"#),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    for permission in ["blacklist.view_blacklist", "blacklist.add_notes"] {
        let res = send(
            &h.app,
            post_json(
                "/api/admin/permissions/grants",
                &owner,
                &format!(
                    r#"{{"permission":"{permission}","group_id":{}}}"#,
                    group["id"]
                ),
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    }
    let officer_account = account_of(&h, &officer).await;
    send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{}/members", group["id"]),
            &owner,
            &format!(r#"{{"account_id":{officer_account}}}"#),
        ),
    )
    .await;
    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=x", &officer),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(
        &h.app,
        form(
            "/blacklist/notes",
            "who=98133756&note=Scammed+a+buyback",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    let res = send(
        &h.app,
        form("/blacklist/notes", "who=98133756&note=Owner+note", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist");
    let notes: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, note FROM core.pilot_notes ORDER BY id")
            .fetch_all(&h.db)
            .await
            .unwrap();
    let log = page(&h, "/blacklist?q=circle", &officer).await.body;
    assert!(
        log.contains("Scammed a buyback") && log.contains("Owner note"),
        "{log}"
    );
    // Their own note, yes; the owner's, no.
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/delete", notes[1].0),
            "",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = send(
        &h.app,
        form(
            &format!("/blacklist/notes/{}/delete", notes[0].0),
            "",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.location(), "/blacklist");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM core.pilot_notes")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(left, 1);
    assert!(
        page(&h, "/dashboard", &officer)
            .await
            .body
            .contains(r#"href="/blacklist""#)
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn any_character_counts_and_leaders_lose_their_groups(db: PgPool) {
    // Guests lead nothing anyway: make the pilot Member.
    cover(&db, Builtin::Member, EntityKind::Character, 443630591).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    // An alt in the corporation about to be blacklisted.
    sqlx::query(
        "INSERT INTO core.characters (id, account_id, name, corporation_id) VALUES (90000002, $1, 'Alt', 98133756)",
    )
    .bind(pilot_account)
    .execute(&h.db)
    .await
    .unwrap();
    // A leader of a group.
    let group = send(
        &h.app,
        post_json(
            "/api/admin/groups",
            &owner,
            r#"{"name":"Haulers","internal":false,"hidden":false}"#,
        ),
    )
    .await;
    let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
    let group = group["id"].as_i64().unwrap();
    let res = send(
        &h.app,
        axum::http::Request::put(format!("/api/admin/groups/{group}/leaders/{pilot_account}"))
            .header(axum::http::header::ORIGIN, SITE)
            .header(axum::http::header::COOKIE, format!("{SESSION}={owner}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert!(res.status.is_success(), "{}", res.body);
    assert_eq!(
        page(&h, &format!("/group-management/{group}"), &pilot)
            .await
            .status,
        StatusCode::OK
    );

    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Spies", &owner),
    )
    .await;
    assert_eq!(res.location(), "/blacklist", "{}", res.body);
    // At once, without waiting for any sync, and though the main is clean.
    assert_eq!(state_of(&h, &pilot).await, "Blacklist");
    assert_ne!(
        page(&h, &format!("/group-management/{group}"), &pilot)
            .await
            .status,
        StatusCode::OK,
        "leading is over too"
    );
    let join = send(&h.app, form(&format!("/groups/{group}/join"), "", &pilot)).await;
    assert_ne!(join.status, StatusCode::SEE_OTHER);
    // Nor can anyone add them.
    let res = send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{group}/members"),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn blacklisting_never_reaches_past_what_you_hold(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let admin = log_in_as(&h, GIGX, None).await; // corp 98133756
    let officer = log_in_as(&h, MITTANI, None).await;
    // gigX holds admin.states; the officer can blacklist but doesn't.
    for (who, permissions) in [
        (&admin, vec!["admin.states"]),
        (
            &officer,
            vec!["blacklist.view_blacklist", "blacklist.manage_blacklist"],
        ),
    ] {
        let account = account_of(&h, who).await;
        let name = format!("g{account}");
        let group = send(
            &h.app,
            post_json(
                "/api/admin/groups",
                &owner,
                &format!(r#"{{"name":"{name}"}}"#),
            ),
        )
        .await;
        let group: serde_json::Value = serde_json::from_str(&group.body).unwrap();
        for permission in permissions {
            send(
                &h.app,
                post_json(
                    "/api/admin/permissions/grants",
                    &owner,
                    &format!(
                        r#"{{"permission":"{permission}","group_id":{}}}"#,
                        group["id"]
                    ),
                ),
            )
            .await;
        }
        send(
            &h.app,
            post_json(
                &format!("/api/admin/groups/{}/members", group["id"]),
                &owner,
                &format!(r#"{{"account_id":{account}}}"#),
            ),
        )
        .await;
    }
    let res = send(
        &h.app,
        form("/blacklist", "who=98133756&reason=Coup", &officer),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("admin.states"), "{}", res.body);
    assert_eq!(state_of(&h, &admin).await, "Member");
}
