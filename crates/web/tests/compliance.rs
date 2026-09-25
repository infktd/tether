#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

//! Scope compliance (F11), Alliance Auth style: a state's required scopes
//! on every character; accounts that fall short keep their state but are
//! flagged and leave the Compliant group; the checklist, the officers'
//! page, and Corp Stats.

mod common;

use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;
use tether_core::states::{Builtin, EntityKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

const CHRIBBA: &str = "196379789:Chribba";
const CHRIBBA_ID: i64 = 196379789;
const CHRIBBA_CORP: i64 = 1164409536;
const MITTANI: &str = "443630591:The Mittani";
const SKILLS: &str = "esi-skills.read_skills.v1";

async fn run_jobs(h: &Harness) {
    let mut registry = tether_jobs::Registry::new();
    tether_web::states::register_jobs(&mut registry, h.db.clone(), h.esi.clone());
    tether_web::compliance::register_jobs(
        &mut registry,
        h.db.clone(),
        h.esi.clone(),
        h.vault.clone(),
    );
    let config = tether_jobs::WorkerConfig::default();
    while tether_jobs::run_once(&h.db, &registry, &config)
        .await
        .unwrap()
        != tether_jobs::Outcome::Idle
    {}
}

/// Whether the account is compliant, and in the Compliant group.
async fn compliance(h: &Harness, token: &str) -> (bool, bool) {
    let me = me(h, token).await;
    let account = me["account_id"].as_i64().unwrap();
    let compliant: bool = sqlx::query_scalar("SELECT compliant FROM core.accounts WHERE id = $1")
        .bind(account)
        .fetch_one(&h.db)
        .await
        .unwrap();
    let in_group = me["groups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g == "Compliant");
    (compliant, in_group)
}

/// The SSO round trip a button starts; returns the new session.
async fn round_trip(h: &Harness, token: &str, uri: &str, character: &str) -> String {
    let res = send(&h.app, form(uri, "", token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.cookie_value(LOGIN);
    let state = query_param(res.location(), "state").to_owned();
    let res = send(
        &h.app,
        get(
            &format!(
                "/auth/callback?code=ok:{}&state={state}",
                character.replace(' ', "%20")
            ),
            &[(LOGIN, &login), (SESSION, token)],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    res.cookie_value(SESSION)
}

async fn audit_details(db: &PgPool, action: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT details FROM core.audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn guests_grant_nothing_and_have_nothing_to_register(db: PgPool) {
    let h = harness(db, true).await;
    let guest = log_in_as(&h, MITTANI, None).await;
    assert!(h.sso.last_requested.lock().unwrap().is_empty());
    let checklist = page(&h, "/register", &guest).await.body;
    assert!(checklist.contains("You're Guest"), "{checklist}");
    // Guest can't be made to require anything.
    let owner = log_in_owner(&h, CHRIBBA).await;
    let res = send(
        &h.app,
        form(
            &format!("/admin/states/{GUEST_STATE}/scopes"),
            &format!("scope={SKILLS}&confirm=1"),
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    // Nor a state require corporation scopes (they need in-game roles).
    let res = send(
        &h.app,
        form(
            &format!("/admin/states/{MEMBER_STATE}/scopes"),
            "scope=esi-wallet.read_corporation_wallets.v1&confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn every_character_must_register_to_be_compliant(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // The Mittani is Chribba's alt.
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(state_of(&h, &owner).await, "Member");
    assert_eq!(compliance(&h, &owner).await, (true, true));

    // Requiring a scope asks first: it would flag Chribba's account.
    let add = format!("/admin/states/{MEMBER_STATE}/scopes");
    let asked = send(&h.app, form(&add, &format!("scope={SKILLS}"), &owner)).await;
    assert_eq!(asked.status, StatusCode::OK);
    assert!(
        asked
            .body
            .contains("Require esi-skills.read_skills.v1 for Member?"),
        "{}",
        asked.body
    );
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(states.contains("Nothing: pilots only need to log in."));
    let applied = send(
        &h.app,
        form(&add, &format!("scope={SKILLS}&confirm=1"), &owner),
    )
    .await;
    assert_eq!(applied.location(), "/admin/states");
    run_jobs(&h).await;
    // Flagged, not demoted (as in Alliance Auth).
    assert_eq!(state_of(&h, &owner).await, "Member");
    assert_eq!(compliance(&h, &owner).await, (false, false));
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(states.contains("Read skills and attributes"), "{states}");
    assert!(states.contains(SKILLS), "listed for the EVE application");

    // The pilot sees a banner and a checklist; officers see the account.
    let profile = page(&h, "/profile", &owner).await.body;
    assert!(profile.contains("Register characters"), "{profile}");
    assert!(profile.contains("Needs registering"), "{profile}");
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(checklist.contains("Register Chribba") && checklist.contains("Register The Mittani"));
    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(officers.contains("Not registered yet"), "{officers}");

    // One character isn't enough.
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    assert_eq!(compliance(&h, &owner).await, (false, false));
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(!checklist.contains("Register Chribba"));
    assert!(checklist.contains("Register The Mittani"));
    // Both are.
    let owner = round_trip(&h, &owner, "/register/start", MITTANI).await;
    assert_eq!(state_of(&h, &owner).await, "Member");
    assert_eq!(compliance(&h, &owner).await, (true, true));
    assert!(
        page(&h, "/compliance", &owner)
            .await
            .body
            .contains("Everyone is compliant.")
    );
    assert!(
        !page(&h, "/profile", &owner)
            .await
            .body
            .contains("Register characters")
    );

    // Both compliance changes and the scope change were audited; the state
    // never changed.
    let changes = audit_details(&h.db, "compliance.change").await;
    assert_eq!(
        changes,
        [
            serde_json::json!({"compliant": false}),
            serde_json::json!({"compliant": true})
        ]
    );
    assert!(
        audit_details(&h.db, "state.change")
            .await
            .iter()
            .all(|d| d["to"] != "Guest")
    );
    let added = audit_details(&h.db, "state.scope_add").await;
    assert_eq!(added[0]["scope"], SKILLS);

    // Officers only.
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(
        page(&h, "/compliance", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&h.app, get("/compliance", &[])).await.location(),
        "/login"
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn corp_stats_lists_members_who_never_registered(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;

    // Chribba's corporation has pilots here but no member list yet.
    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(officers.contains("No member list yet"), "{officers}");

    // Chribba shares it from the profile; it waits for an admin.
    let owner = round_trip(&h, &owner, "/profile/corp-stats/offer", CHRIBBA).await;
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(
        asked.contains(&tether_core::scopes::CORP_MEMBERSHIP.to_owned()),
        "{asked:?}"
    );
    assert!(
        page(&h, "/profile", &owner)
            .await
            .body
            .contains("waiting for an admin")
    );

    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/members")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([CHRIBBA_ID, 90000011, 90000012])),
        )
        .mount(&h.esi_server)
        .await;

    // Approving needs admin.states, not just being signed in.
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    let approve = format!("/compliance/sources/{CHRIBBA_ID}/approve");
    assert_eq!(
        send(&h.app, form(&approve, "", &pilot)).await.status,
        StatusCode::FORBIDDEN
    );
    let approved = send(&h.app, form(&approve, "", &owner)).await;
    assert_eq!(approved.location(), "/compliance");
    run_jobs(&h).await;

    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(officers.contains("2 never registered"), "{officers}");
    assert!(officers.contains("Character 90000011"), "{officers}");
    assert!(!officers.contains("No member list yet"), "{officers}");
    let lists: i64 = sqlx::query_scalar("SELECT members::bigint FROM core.corp_member_lists")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(lists, 3);

    // Withdrawing stops it; the list goes at the next run.
    let withdrawn = send(
        &h.app,
        form(
            &format!("/profile/corp-stats/{CHRIBBA_ID}/withdraw"),
            "",
            &owner,
        ),
    )
    .await;
    assert_eq!(withdrawn.location(), "/profile");
    tether_web::compliance::corp_stats(&h.db, &h.esi, &h.vault)
        .await
        .unwrap();
    let lists: i64 = sqlx::query_scalar("SELECT count(*) FROM core.corp_member_lists")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(lists, 0);
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM core.audit_log WHERE action LIKE 'corp_stats.%' ORDER BY id",
    )
    .fetch_all(&h.db)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "corp_stats.offered",
            "corp_stats.approved",
            "corp_stats.withdrawn"
        ]
    );
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tether_manages_the_compliant_group(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let group: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE managed = 'compliant'")
        .fetch_one(&h.db)
        .await
        .unwrap();
    // Guests aren't in it; nobody edits it by hand, not even the owner.
    let guest = log_in_as(&h, MITTANI, None).await;
    assert_eq!(compliance(&h, &guest).await, (true, false));
    let account = me(&h, &guest).await["account_id"].as_i64().unwrap();
    for (uri, body) in [
        (
            format!("/admin/groups/{group}/members"),
            "character=the+mittani".to_owned(),
        ),
        (
            format!("/admin/groups/{group}/members/{account}/remove"),
            String::new(),
        ),
        (format!("/admin/groups/{group}/delete"), String::new()),
    ] {
        let res = send(&h.app, form(&uri, &body, &owner)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{uri}: {}", res.body);
    }
    let leave = send(
        &h.app,
        post_json(&format!("/api/groups/{group}/leave"), &owner, "{}"),
    )
    .await;
    assert_eq!(leave.status, StatusCode::BAD_REQUEST);
    assert_eq!(compliance(&h, &owner).await, (true, true));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admin_states_is_not_a_way_into_the_compliant_group(db: PgPool) {
    use tether_core::states::StateId;
    use tether_db::groups::GroupId;
    use tether_db::permissions::{Grantee, grant};

    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    // A Guest who manages states (through an assigned group) but isn't an
    // auditor; the Compliant group grants admin.audit.
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    let pilot_account = me(&h, &pilot).await["account_id"].as_i64().unwrap();
    let wranglers = tether_db::groups::create(
        &h.db,
        "Wranglers",
        "",
        tether_core::permissions::JoinPolicy::Assigned,
    )
    .await
    .unwrap();
    tether_db::groups::add_member(
        &h.db,
        wranglers,
        tether_db::accounts::AccountId(pilot_account),
    )
    .await
    .unwrap();
    grant(&h.db, "admin.states", Grantee::Group(wranglers))
        .await
        .unwrap();
    let compliant: i64 =
        sqlx::query_scalar("SELECT id FROM core.groups WHERE managed = 'compliant'")
            .fetch_one(&h.db)
            .await
            .unwrap();
    grant(&h.db, "admin.audit", Grantee::Group(GroupId(compliant)))
        .await
        .unwrap();

    // Covering their own corporation with Blue would put them in the
    // Compliant group, so it's refused.
    let res = send(
        &h.app,
        form(
            &format!("/admin/states/{BLUE_STATE}/covers"),
            "entity_id=98133756&confirm=1",
            &pilot,
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert!(res.body.contains("admin.audit"), "{}", res.body);
    // The owner can.
    let res = send(
        &h.app,
        form(
            &format!("/admin/states/{BLUE_STATE}/covers"),
            "entity_id=98133756&confirm=1",
            &owner,
        ),
    )
    .await;
    assert_eq!(res.location(), "/admin/states");
    run_jobs(&h).await;
    assert_eq!(compliance(&h, &pilot).await, (true, true));
    // Joining it was audited.
    let added = audit_details(&h.db, "group.member.add").await;
    assert!(
        added
            .iter()
            .any(|d| d["account_id"] == pilot_account && d["reason"] == "compliance"),
        "{added:?}"
    );
    let _ = StateId(BLUE_STATE);
}
