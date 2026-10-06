//! Secure Groups (aa-securegroups): smart groups whose members Tether keeps
//! by filters, with auto join, requests, grace periods and notifications.

use axum::http::StatusCode;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use crate::common::*;

const CHRIBBA: &str = "196379789:Chribba"; // corp 1164409536, alliance 159826257
const GIGX: &str = "1887431749:gigX"; // corp 98133756, alliance 1695357456
const MITTANI: &str = "443630591:The Mittani"; // NPC corp, no alliance

async fn account_of(h: &Harness, token: &str) -> i64 {
    me(h, token).await["account_id"].as_i64().unwrap()
}

async fn group(h: &Harness, owner: &str, body: &str) -> i64 {
    let res = send(&h.app, post_json("/api/admin/groups", owner, body)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    serde_json::from_str::<serde_json::Value>(&res.body).unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn smart(h: &Harness, owner: &str, id: i64, body: &str) -> Res {
    send(
        &h.app,
        form(&format!("/admin/groups/{id}/smart"), body, owner),
    )
    .await
}

async fn filter(h: &Harness, owner: &str, id: i64, body: &str) -> Res {
    send(
        &h.app,
        form(&format!("/admin/groups/{id}/smart/filters"), body, owner),
    )
    .await
}

async fn sweep(h: &Harness) -> usize {
    tether_web::smart_groups::sweep(&h.db, &h.esi)
        .await
        .unwrap()
}

async fn in_group(h: &Harness, group: i64, account: i64) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM core.group_members WHERE group_id = $1 AND account_id = $2)",
    )
    .bind(group)
    .bind(account)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn auto_groups_follow_their_filters(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let guest = log_in_as(&h, MITTANI, None).await;
    let (pilot_account, guest_account) =
        (account_of(&h, &pilot).await, account_of(&h, &guest).await);
    let miners = group(
        &h,
        &owner,
        r#"{"name":"Miners","internal":false,"hidden":false}"#,
    )
    .await;

    assert_eq!(
        smart(
            &h,
            &pilot,
            miners,
            "smart=on&configured=on&enabled=on&include_in_updates=on&auto_join=on&notify_on_add=on&notify_on_remove=on&notify_on_grace=on"
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let res = smart(
        &h,
        &owner,
        miners,
        "smart=on&configured=on&enabled=on&include_in_updates=on&auto_join=on&notify_on_add=on&notify_on_remove=on&notify_on_grace=on",
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{miners}"),
        "{}",
        res.body
    );
    // No filters: nobody is added (that would be everyone).
    sweep(&h).await;
    assert!(!in_group(&h, miners, pilot_account).await);
    let res = filter(
        &h,
        &owner,
        miners,
        &format!("kind=state&states={MEMBER_STATE}"),
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{miners}"),
        "{}",
        res.body
    );
    assert_eq!(sweep(&h).await, 1);
    assert!(in_group(&h, miners, pilot_account).await);
    assert!(!in_group(&h, miners, guest_account).await);
    let note: String = sqlx::query_scalar(
        "SELECT title FROM core.notifications WHERE account_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(pilot_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(note, "Added to Miners");
    // The group's page shows the requirement.
    let page_body = page(&h, "/groups", &pilot).await.body;
    assert!(
        page_body.contains("Requires: state is Member"),
        "{page_body}"
    );

    // Leaving Member takes them out at once (no grace period).
    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, 1695357456)
        .await
        .unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(pilot_account))
        .await
        .unwrap();
    assert_eq!(sweep(&h).await, 1);
    assert!(!in_group(&h, miners, pilot_account).await);
    let log: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.group_request_log WHERE group_id = $1 AND request_type = 'removed'",
    )
    .bind(miners)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(log, 1, "in the group's Audit Log");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn a_grace_period_warns_before_removing(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    // Registered with Member's scopes, so compliant.
    let pilot = log_in_as(&h, GIGX, Some(&pilot)).await;
    let pilot_account = account_of(&h, &pilot).await;
    let caps = group(
        &h,
        &owner,
        r#"{"name":"Capitals","internal":false,"hidden":false}"#,
    )
    .await;
    smart(
        &h,
        &owner,
        caps,
        "smart=on&configured=on&enabled=on&include_in_updates=on&auto_join=on&notify_on_add=on&notify_on_remove=on&notify_on_grace=on&can_grace=on",
    )
    .await;
    filter(&h, &owner, caps, "kind=compliant&grace_days=3").await;
    sweep(&h).await;
    assert!(in_group(&h, caps, pilot_account).await);
    // The owner is Guest here: never added unless the group names Guest.
    let owner_account = account_of(&h, &owner).await;
    assert!(!in_group(&h, caps, owner_account).await);

    sqlx::query("UPDATE core.accounts SET compliant = false WHERE id = $1")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(sweep(&h).await, 0, "warned, not removed");
    assert!(in_group(&h, caps, pilot_account).await);
    let note: String = sqlx::query_scalar(
        "SELECT title FROM core.notifications WHERE account_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(pilot_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert!(note.starts_with("Leaving Capitals on"), "{note}");
    // Grace over.
    sqlx::query("UPDATE core.smart_grace SET expires_at = now() - interval '1 hour'")
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(sweep(&h).await, 1);
    assert!(!in_group(&h, caps, pilot_account).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn requests_need_passing_and_filters_are_checked(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    // Open, but only for those with no character in CircleOfTwo Holding.
    let scouts = group(
        &h,
        &owner,
        r#"{"name":"Scouts","internal":false,"hidden":false,"open":true}"#,
    )
    .await;
    smart(&h, &owner, scouts, "smart=on").await;
    let res = filter(
        &h,
        &owner,
        scouts,
        "kind=any_affiliation&entities=98133756&reversed=on",
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{scouts}"),
        "{}",
        res.body
    );
    let join = send(&h.app, form(&format!("/groups/{scouts}/join"), "", &pilot)).await;
    assert_eq!(join.status, StatusCode::FORBIDDEN, "{}", join.body);
    assert!(
        join.body.contains("not a character in CircleOfTwo Holding"),
        "{}",
        join.body
    );

    // Checked filters: nonsense, unknown names, and managed groups.
    for (body, why) in [
        ("kind=nonsense", "Choose a filter"),
        ("kind=state", "Choose at least one"),
        ("kind=character_age&days=0", "age in days"),
        ("kind=main_affiliation&entities=", "1 to 20"),
    ] {
        let res = filter(&h, &owner, scouts, body).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{body}");
        assert!(res.body.contains(why), "{body}: {}", res.body);
    }
    let compliant: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE name = 'Compliant'")
        .fetch_one(&h.db)
        .await
        .unwrap();
    let res = smart(&h, &owner, compliant, "smart=on").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    // Character age, from the main's public birthday.
    Mock::given(method("GET"))
        .and(path("/characters/1887431749"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "gigX", "corporation_id": 98133756, "birthday": "2007-03-01T00:00:00Z",
            "gender": "male", "race_id": 1, "bloodline_id": 1, "security_status": 0.0, "achievement_score": 0,
        })))
        .mount(&h.esi_server)
        .await;
    let vets = group(
        &h,
        &owner,
        r#"{"name":"Veterans","internal":false,"hidden":false}"#,
    )
    .await;
    smart(&h, &owner, vets, "smart=on&auto_join=on").await;
    filter(&h, &owner, vets, "kind=character_age&days=3650").await;
    sweep(&h).await;
    let pilot_account = account_of(&h, &pilot).await;
    assert!(
        in_group(&h, vets, pilot_account).await,
        "a 2007 character is over ten years old"
    );

    // Back to ordinary: filters and grace go.
    let res = smart(&h, &owner, vets, "").await;
    assert_eq!(res.location(), format!("/admin/groups/{vets}"));
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.smart_filters WHERE group_id = $1")
            .bind(vets)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert_eq!(left, 0);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM core.audit_log WHERE action LIKE 'smart_group.%'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(audited >= 5, "{audited}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn smart_groups_never_reach_past_what_you_hold(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let admin = log_in_as(&h, GIGX, None).await;
    let admin_account = account_of(&h, &admin).await;
    // A group admin holding admin.groups only.
    let staff = group(&h, &owner, r#"{"name":"Group admins"}"#).await;
    send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"admin.groups","group_id":{staff}}}"#),
        ),
    )
    .await;
    send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{staff}/members"),
            &owner,
            &format!(r#"{{"account_id":{admin_account}}}"#),
        ),
    )
    .await;
    // A group granting admin.permissions.
    let directors = group(
        &h,
        &owner,
        r#"{"name":"Directors","internal":false,"hidden":false}"#,
    )
    .await;
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"admin.permissions","group_id":{directors}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    // Making it smart would let a filter they pass add them: refused.
    let res = smart(&h, &admin, directors, "smart=on&auto_join=on").await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("admin.permissions"), "{}", res.body);

    // An Internal smart group stays hidden: the same 404 as none.
    let secret = group(&h, &owner, r#"{"name":"Secret"}"#).await;
    smart(&h, &owner, secret, "smart=on").await;
    filter(&h, &owner, secret, "kind=compliant&reversed=on").await;
    let join = send(&h.app, form(&format!("/groups/{secret}/join"), "", &admin)).await;
    assert_eq!(join.status, StatusCode::NOT_FOUND, "{}", join.body);
    // And a filter naming an Internal group doesn't name it to pilots.
    let scouts = group(
        &h,
        &owner,
        r#"{"name":"Scouts","internal":false,"hidden":false}"#,
    )
    .await;
    smart(&h, &owner, scouts, "smart=on").await;
    filter(
        &h,
        &owner,
        scouts,
        &format!("kind=groups&groups={secret}&match=any"),
    )
    .await;
    let listed = page(&h, "/groups", &admin).await.body;
    assert!(listed.contains("in any of: a private group"), "{listed}");
    assert!(!listed.contains("of: Secret"), "{listed}");
    // Nor can a filter name its own group.
    let res = filter(
        &h,
        &owner,
        scouts,
        &format!("kind=groups&groups={scouts}&match=any"),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn huge_app_values_never_stop_sweeps(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    sqlx::query("INSERT INTO core.characters (id, account_id, name) VALUES (90000003, $1, 'Alt')")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    // Values that would overflow a sum, however they got there.
    sqlx::query(
        "INSERT INTO core.plugin_filter_reports (plugin_id, name, config) VALUES ('x', 'y', '{}')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    for character in [1887431749_i64, 90000003] {
        sqlx::query(
            "INSERT INTO core.plugin_filter_values (plugin_id, name, config, character_id, value) \
             VALUES ('x', 'y', '{}', $1, 9223372036854775807)",
        )
        .bind(character)
        .execute(&h.db)
        .await
        .unwrap();
    }
    let miners = group(
        &h,
        &owner,
        r#"{"name":"Miners","internal":false,"hidden":false}"#,
    )
    .await;
    smart(&h, &owner, miners, "smart=on&auto_join=on").await;
    filter(
        &h,
        &owner,
        miners,
        &format!("kind=state&states={MEMBER_STATE}"),
    )
    .await;
    assert_eq!(sweep(&h).await, 1, "other groups carry on");
    assert!(in_group(&h, miners, pilot_account).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn check_now_runs_one_group_and_check_shows_each_filter(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    log_in_as(&h, MITTANI, None).await;
    let owner_account = account_of(&h, &owner).await;
    let pilot_account = account_of(&h, &pilot).await;
    let mut ids = Vec::new();
    for name in ["Miners", "Haulers"] {
        let id = group(
            &h,
            &owner,
            &format!(r#"{{"name":"{name}","internal":false,"hidden":false}}"#),
        )
        .await;
        smart(
            &h,
            &owner,
            id,
            "smart=on&configured=on&enabled=on&include_in_updates=on&auto_join=on&notify_on_add=on&notify_on_remove=on&notify_on_grace=on",
        )
        .await;
        filter(&h, &owner, id, &format!("kind=state&states={MEMBER_STATE}")).await;
        ids.push(id);
    }
    let (miners, haulers) = (ids[0], ids[1]);
    let check_now = format!("/admin/groups/{miners}/smart/check");

    // Only group admins.
    let res = send(&h.app, form(&check_now, "", &pilot)).await;
    assert!(
        matches!(res.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}",
        res.status
    );
    assert!(!in_group(&h, miners, pilot_account).await);

    // Check now judges this group only, at once, and is audited: the
    // check and each change, as the admin's.
    let res = send(&h.app, form(&check_now, "", &owner)).await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{miners}?checked=1"),
        "{}",
        res.body
    );
    let shown = page(&h, res.location(), &owner).await.body;
    assert!(
        shown.contains("Checked now: 1 membership changed."),
        "{shown}"
    );
    assert!(in_group(&h, miners, pilot_account).await);
    assert!(!in_group(&h, haulers, pilot_account).await);
    let audited: Vec<(Option<i64>, String, String)> = sqlx::query_as(
        "SELECT actor_account_id, action, details::text FROM core.audit_log \
         WHERE target = $1 AND action IN ('smart_group.check_now', 'group.member.add') \
         ORDER BY id",
    )
    .bind(format!("group:{miners}"))
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(audited.len(), 2, "{audited:?}");
    assert_eq!(audited[0].0, Some(owner_account));
    assert_eq!(audited[0].1, "group.member.add");
    assert_eq!(audited[1].0, Some(owner_account));
    assert_eq!(audited[1].1, "smart_group.check_now");
    assert!(audited[1].2.contains(r#""changed": 1"#), "{}", audited[1].2);
    // Not again straight away: each check reads every account.
    let again = send(&h.app, form(&check_now, "", &owner)).await;
    assert_eq!(
        again.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        again.body
    );
    sqlx::query("UPDATE core.smart_groups SET swept_at = now() - interval '1 minute'")
        .execute(&h.db)
        .await
        .unwrap();
    let again = send(&h.app, form(&check_now, "", &owner)).await;
    assert_eq!(
        again.location(),
        format!("/admin/groups/{miners}?checked=0")
    );
    let shown = page(&h, again.location(), &owner).await.body;
    assert!(shown.contains("every member passes"), "{shown}");

    // The group's page: when it was last checked, and Check on each row.
    let page_body = page(&h, &format!("/admin/groups/{miners}"), &owner)
        .await
        .body;
    assert!(page_body.contains("last at"), "{page_body}");
    assert!(
        page_body.contains(&format!("?account={pilot_account}#check")),
        "{page_body}"
    );

    // Check: each filter, pass or fail, for one pilot; audited.
    let passes = page(&h, &format!("/admin/groups/{miners}?check=gigX"), &owner).await;
    assert_eq!(passes.status, StatusCode::OK, "{}", passes.body);
    for part in [
        r#"<h3 class="font-medium" id="check-title">gigX</h3>"#,
        r#"<span class="status-line" data-tone="ok">Passes</span>"#,
        r#"<span class="status-line" data-tone="ok">Pass</span><span>state is Member</span>"#,
        r#"<span class="status-line" data-tone="ok">Member</span>"#,
    ] {
        assert!(passes.body.contains(part), "{part}\n\n{}", passes.body);
    }
    let checks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.audit_log WHERE action = 'smart_group.check' \
         AND actor_account_id = $1 AND (details->>'account_id')::bigint = $2",
    )
    .bind(owner_account)
    .bind(pilot_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(checks, 1);
    let by_row = page(
        &h,
        &format!("/admin/groups/{miners}?account={pilot_account}"),
        &owner,
    )
    .await;
    assert!(
        by_row.body.contains("id=\"check-title\">gigX</h3>"),
        "{}",
        by_row.body
    );
    // Someone outside the group: for those who may look up any account.
    let fails = page(
        &h,
        &format!("/admin/groups/{miners}?check=The%20Mittani"),
        &owner,
    )
    .await;
    for part in [
        r#"<span class="status-line" data-tone="danger">Fails</span>"#,
        r#"<span class="status-line" data-tone="danger">Fail</span><span>state is Member</span>"#,
        r#"<span class="status-line" data-tone="off">Not a member</span>"#,
    ] {
        assert!(fails.body.contains(part), "{part}\n\n{}", fails.body);
    }
    let nobody = page(&h, &format!("/admin/groups/{miners}?check=Nobody"), &owner).await;
    assert!(
        nobody
            .body
            .contains("No account has a character with that name."),
        "{}",
        nobody.body
    );
    // Not for pilots.
    let res = page(&h, &format!("/admin/groups/{miners}?check=gigX"), &pilot).await;
    assert!(!res.body.contains("check-title"), "{}", res.body);

    // A group admin without admin.users checks the group's own members
    // only: nobody else's main or standing.
    let staff = group(&h, &owner, r#"{"name":"Group admins"}"#).await;
    send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            &owner,
            &format!(r#"{{"permission":"admin.groups","group_id":{staff}}}"#),
        ),
    )
    .await;
    send(
        &h.app,
        post_json(
            &format!("/api/admin/groups/{staff}/members"),
            &owner,
            &format!(r#"{{"account_id":{pilot_account}}}"#),
        ),
    )
    .await;
    let own = page(&h, &format!("/admin/groups/{miners}?check=gigX"), &pilot).await;
    assert!(own.body.contains("check-title"), "{}", own.body);
    let other = page(
        &h,
        &format!("/admin/groups/{miners}?check=The%20Mittani"),
        &pilot,
    )
    .await;
    assert_eq!(other.status, StatusCode::FORBIDDEN, "{}", other.body);
    assert!(!other.body.contains("check-title"), "{}", other.body);
    assert!(other.body.contains("admin.users"), "{}", other.body);

    // An ordinary group has nothing to check.
    let plain = group(
        &h,
        &owner,
        r#"{"name":"Plain","internal":false,"hidden":false}"#,
    )
    .await;
    let res = send(
        &h.app,
        form(&format!("/admin/groups/{plain}/smart/check"), "", &owner),
    )
    .await;
    assert!(
        res.body
            .contains("Only a smart group has filters to check."),
        "{}",
        res.body
    );
}

/// All of allianceauth-secure-groups' settings, as the full form posts
/// them; add `&can_grace=on` and the rest as needed.
const CONFIGURED: &str = "smart=on&configured=on&enabled=on&include_in_updates=on&auto_join=on";

/// The account's notifications about Secure Groups.
async fn notifications(h: &Harness, account: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT title FROM core.notifications WHERE account_id = $1 \
         AND (title LIKE 'Added to %' OR title LIKE 'Removed from %' OR title LIKE 'Leaving %') \
         ORDER BY id",
    )
    .bind(account)
    .fetch_all(&h.db)
    .await
    .unwrap()
}

async fn grant_state(h: &Harness, owner: &str, permission: &str, state: i64) {
    let res = send(
        &h.app,
        post_json(
            "/api/admin/permissions/grants",
            owner,
            &format!(r#"{{"permission":"{permission}","state_id":{state}}}"#),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn aa_settings_decide_notifications_updates_and_whether_it_runs(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot_account = account_of(&h, &pilot).await;
    let miners = group(
        &h,
        &owner,
        r#"{"name":"Miners","internal":false,"hidden":false,"open":true}"#,
    )
    .await;
    // Made smart from the plain form: AA's defaults.
    smart(&h, &owner, miners, "smart=on&auto_join=on").await;
    let defaults: (bool, bool, bool, bool, bool, bool) = sqlx::query_as(
        "SELECT enabled, include_in_updates, can_grace, notify_on_add, notify_on_remove, \
         notify_on_grace FROM core.smart_groups WHERE group_id = $1",
    )
    .bind(miners)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(defaults, (true, true, false, false, true, true));
    filter(
        &h,
        &owner,
        miners,
        &format!("kind=state&states={MEMBER_STATE}"),
    )
    .await;
    // Added, and not told (notify on add is off).
    assert_eq!(sweep(&h).await, 1);
    assert!(in_group(&h, miners, pilot_account).await);
    assert!(notifications(&h, pilot_account).await.is_empty());

    // Out of the hourly updates: the sweep leaves it; Check now doesn't.
    let res = smart(
        &h,
        &owner,
        miners,
        "smart=on&configured=on&enabled=on&auto_join=on&notify_on_remove=on",
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{miners}"),
        "{}",
        res.body
    );
    let (rule,): (i64,) = sqlx::query_as("SELECT id FROM core.smart_filters WHERE group_id = $1")
        .bind(miners)
        .fetch_one(&h.db)
        .await
        .unwrap();
    send(
        &h.app,
        form(
            &format!("/admin/groups/{miners}/smart/filters/{rule}/delete"),
            "",
            &owner,
        ),
    )
    .await;
    filter(
        &h,
        &owner,
        miners,
        &format!("kind=state&states={BLUE_STATE}"),
    )
    .await;
    assert_eq!(sweep(&h).await, 0);
    assert!(in_group(&h, miners, pilot_account).await);
    sqlx::query("UPDATE core.smart_groups SET swept_at = NULL")
        .execute(&h.db)
        .await
        .unwrap();
    let res = send(
        &h.app,
        form(&format!("/admin/groups/{miners}/smart/check"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(!in_group(&h, miners, pilot_account).await);
    assert_eq!(
        notifications(&h, pilot_account).await,
        vec!["Removed from Miners".to_owned()]
    );

    // Switched off, it's an ordinary group: joining ignores the filters,
    // and nothing sweeps it.
    smart(&h, &owner, miners, "smart=on&configured=on").await;
    let join = send(&h.app, form(&format!("/groups/{miners}/join"), "", &pilot)).await;
    assert!(
        join.status.is_success() || join.status.is_redirection(),
        "{}",
        join.body
    );
    assert!(in_group(&h, miners, pilot_account).await);
    let res = send(
        &h.app,
        form(&format!("/admin/groups/{miners}/smart/check"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert!(res.body.contains("switched off"), "{}", res.body);
    sqlx::query("UPDATE core.smart_groups SET swept_at = NULL")
        .execute(&h.db)
        .await
        .unwrap();
    sweep(&h).await;
    assert!(in_group(&h, miners, pilot_account).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn grace_is_per_filter(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot = log_in_as(&h, GIGX, Some(&pilot)).await;
    let pilot_account = account_of(&h, &pilot).await;
    let caps = group(
        &h,
        &owner,
        r#"{"name":"Capitals","internal":false,"hidden":false}"#,
    )
    .await;
    smart(
        &h,
        &owner,
        caps,
        &format!("{CONFIGURED}&can_grace=on&notify_on_grace=on"),
    )
    .await;
    // Member: no grace. Compliant: three days.
    filter(
        &h,
        &owner,
        caps,
        &format!("kind=state&states={MEMBER_STATE}&grace_days=0"),
    )
    .await;
    filter(&h, &owner, caps, "kind=compliant&grace_days=3").await;
    // Out of range.
    let res = filter(&h, &owner, caps, "kind=compliant&grace_days=61").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    sweep(&h).await;
    assert!(in_group(&h, caps, pilot_account).await);

    sqlx::query("UPDATE core.accounts SET compliant = false WHERE id = $1")
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(sweep(&h).await, 0, "compliant has a grace period");
    let graced: Vec<(String, f64)> = sqlx::query_as(
        "SELECT f.kind, EXTRACT(epoch FROM g.expires_at - now())::float8 / 86400 \
         FROM core.smart_grace g JOIN core.smart_filters f ON f.id = g.filter_id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(graced.len(), 1, "{graced:?}");
    assert_eq!(graced[0].0, "compliant");
    assert!((2.9..3.1).contains(&graced[0].1), "{graced:?}");
    assert!(
        notifications(&h, pilot_account)
            .await
            .iter()
            .any(|n| n.starts_with("Leaving Capitals on"))
    );
    // Leaving Member has none: out at once.
    let member = tether_db::states::builtin(&h.db, Builtin::Member)
        .await
        .unwrap()
        .unwrap();
    tether_db::states::remove_entity(&h.db, member.id, 1695357456)
        .await
        .unwrap();
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(pilot_account))
        .await
        .unwrap();
    assert_eq!(sweep(&h).await, 1);
    assert!(!in_group(&h, caps, pilot_account).await);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn expressions_exemptions_factions_and_services(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, GIGX, None).await;
    let pilot = log_in_as(&h, GIGX, Some(&pilot)).await;
    let pilot_account = account_of(&h, &pilot).await;
    let new_group = |name: &'static str| {
        let (h, owner) = (&h, &owner);
        async move {
            let id = group(
                h,
                owner,
                &format!(r#"{{"name":"{name}","internal":false,"hidden":false}}"#),
            )
            .await;
            smart(h, owner, id, CONFIGURED).await;
            id
        }
    };

    // Blue OR compliant: compliant is enough.
    let either = new_group("Either").await;
    filter(
        &h,
        &owner,
        either,
        &format!("kind=state&states={BLUE_STATE}"),
    )
    .await;
    filter(&h, &owner, either, "kind=compliant").await;
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM core.smart_filters WHERE group_id = $1 ORDER BY id")
            .bind(either)
            .fetch_all(&h.db)
            .await
            .unwrap();
    let res = send(
        &h.app,
        form(
            &format!("/admin/groups/{either}/smart/filters/combine"),
            &format!("first={}&second={}&operator=or", ids[0], ids[1]),
            &owner,
        ),
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{either}"),
        "{}",
        res.body
    );
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM core.smart_filters WHERE group_id = $1")
            .bind(either)
            .fetch_all(&h.db)
            .await
            .unwrap();
    assert_eq!(kinds, vec!["expression".to_owned()]);
    let admin_page = page(&h, &format!("/admin/groups/{either}"), &owner)
        .await
        .body;
    assert!(
        admin_page.contains("(state is Blue OR compliant)"),
        "{admin_page}"
    );

    // An alt in CircleOfTwo Holding keeps you out, unless your main's
    // alliance is exempt.
    let exempt = new_group("Exempt").await;
    let res = filter(
        &h,
        &owner,
        exempt,
        "kind=any_affiliation&entities=98133756&reversed=on&exempt=1695357456",
    )
    .await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{exempt}"),
        "{}",
        res.body
    );

    // Caldari militia, on any character.
    sqlx::query(
        "INSERT INTO core.entity_names (id, name, category) VALUES (500001, 'Caldari State', 'faction')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    let militia = new_group("Militia").await;
    let res = filter(&h, &owner, militia, "kind=faction&factions=500001").await;
    assert_eq!(
        res.location(),
        format!("/admin/groups/{militia}"),
        "{}",
        res.body
    );

    // Linked to Discord (AA's service filters: Discord is Tether's one).
    let linked = new_group("Linked").await;
    filter(&h, &owner, linked, "kind=service&service=discord").await;

    sweep(&h).await;
    assert!(in_group(&h, either, pilot_account).await);
    assert!(in_group(&h, exempt, pilot_account).await);
    assert!(!in_group(&h, militia, pilot_account).await);
    assert!(!in_group(&h, linked, pilot_account).await);

    sqlx::query("UPDATE core.characters SET faction_id = 500001 WHERE id = 1887431749")
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO core.discord_links (account_id, discord_user_id, username) VALUES ($1, 42, 'gigx')",
    )
    .bind(pilot_account)
    .execute(&h.db)
    .await
    .unwrap();
    sweep(&h).await;
    assert!(in_group(&h, militia, pilot_account).await);
    assert!(in_group(&h, linked, pilot_account).await);
    let admin_page = page(&h, &format!("/admin/groups/{exempt}"), &owner)
        .await
        .body;
    assert!(
        admin_page.contains(
            "not a character in CircleOfTwo Holding, unless the main is in Circle-Of-Two"
        ),
        "{admin_page}"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn the_secure_groups_page_and_its_audit_follow_aas_permissions(db: PgPool) {
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let pilot = log_in_as(&h, MITTANI, None).await; // Guest
    let pilot_account = account_of(&h, &pilot).await;
    let officer = log_in_as(&h, GIGX, None).await; // Guest
    let officer_account = account_of(&h, &officer).await;
    let scouts = group(
        &h,
        &owner,
        r#"{"name":"Scouts","internal":false,"hidden":true}"#,
    )
    .await;
    smart(&h, &owner, scouts, "smart=on").await;
    filter(
        &h,
        &owner,
        scouts,
        &format!("kind=state&states={GUEST_STATE}"),
    )
    .await;
    let secret = group(&h, &owner, r#"{"name":"Secret"}"#).await;
    smart(&h, &owner, secret, "smart=on").await;

    assert_eq!(
        page(&h, "/securegroups", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    grant_state(&h, &owner, "securegroups.access_sec_group", GUEST_STATE).await;
    let listed = page(&h, "/securegroups", &pilot).await.body;
    assert!(
        listed.contains("Scouts"),
        "hidden, but a Secure Group: {listed}"
    );
    assert!(!listed.contains("Secret"), "Internal: {listed}");
    assert!(listed.contains("state is Guest"), "{listed}");
    // Guests can't request groups, but the Secure Groups page asks for
    // them (AA).
    let res = send(
        &h.app,
        form(&format!("/securegroups/{scouts}/join"), "", &pilot),
    )
    .await;
    assert_eq!(res.location(), "/securegroups", "{}", res.body);
    let requested: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.group_requests WHERE group_id = $1 AND account_id = $2",
    )
    .bind(scouts)
    .bind(pilot_account)
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(requested, 1);
    let res = send(
        &h.app,
        form(&format!("/securegroups/{secret}/join"), "", &pilot),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    // The audit needs its permission and Group Management over the group.
    sqlx::query("INSERT INTO core.group_members (group_id, account_id) VALUES ($1, $2)")
        .bind(scouts)
        .bind(pilot_account)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        page(&h, "/securegroups/audit", &officer).await.status,
        StatusCode::FORBIDDEN
    );
    grant_state(&h, &owner, "securegroups.audit_sec_group", GUEST_STATE).await;
    let list = page(&h, "/securegroups/audit", &officer).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(!list.body.contains("Scouts"), "{}", list.body);
    assert_eq!(
        page(&h, &format!("/securegroups/audit/{scouts}"), &officer)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Group Management (for Member), with the officer made Member.
    grant_state(&h, &owner, "group_management", MEMBER_STATE).await;
    cover(&h.db, Builtin::Member, EntityKind::Alliance, 1695357456).await;
    tether_web::states::evaluate_account(&h.db, tether_db::accounts::AccountId(officer_account))
        .await
        .unwrap();
    grant_state(&h, &owner, "securegroups.audit_sec_group", MEMBER_STATE).await;
    let list = page(&h, "/securegroups/audit", &officer).await.body;
    assert!(list.contains("Scouts"), "{list}");
    let audit = page(&h, &format!("/securegroups/audit/{scouts}"), &officer).await;
    assert_eq!(audit.status, StatusCode::OK, "{}", audit.body);
    assert!(audit.body.contains("The Mittani"), "{}", audit.body);
    assert!(audit.body.contains("state is Guest"), "{}", audit.body);
    let res = send(
        &h.app,
        form(&format!("/securegroups/audit/{scouts}/check"), "", &officer),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let res = send(
        &h.app,
        form(
            &format!("/securegroups/audit/{scouts}/members/{pilot_account}/remove"),
            "",
            &officer,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(!in_group(&h, scouts, pilot_account).await);
}
