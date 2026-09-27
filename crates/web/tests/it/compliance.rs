//! Scope compliance (F11), Alliance Auth style: a state's required scopes
//! on every character; accounts that fall short keep their state but are
//! flagged and leave the Compliant group; the checklist, the officers'
//! page, and Corporation Stats.

use crate::common::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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
    // The Mittani is Chribba's alt, added (and so registered) with the
    // scopes Member requires; Chribba only logged in.
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(state_of(&h, &owner).await, "Member");
    assert_eq!(compliance(&h, &owner).await, (false, false));
    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(officers.contains("Not registered yet"), "{officers}");
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
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
    let profile = page(&h, "/dashboard", &owner).await.body;
    assert!(profile.contains("Register Character"), "{profile}");
    assert!(profile.contains("Missing 1 scope · Register"), "{profile}");
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(checklist.contains("Register Chribba") && checklist.contains("Register The Mittani"));
    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(
        officers.contains("Missing Read skills and attributes"),
        "{officers}"
    );

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
        !page(&h, "/dashboard", &owner)
            .await
            .body
            .contains("Register Character")
    );

    // Every compliance change and the scope change were audited; the state
    // never changed.
    let changes = audit_details(&h.db, "compliance.change").await;
    assert_eq!(
        changes,
        [
            serde_json::json!({"compliant": false}),
            serde_json::json!({"compliant": true}),
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

/// Waits for the background work after a registration to queue a read of
/// the corporation's member list.
async fn wait_for_member_list_job(h: &Harness, corporation: i64) {
    for _ in 0..200 {
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM core.jobs WHERE kind = 'compliance.corp_stats' \
             AND (payload->>'corporation_id')::bigint = $1",
        )
        .bind(corporation)
        .fetch_one(&h.db)
        .await
        .unwrap();
        if queued > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no member list read queued for {corporation}");
}

async fn member_list_source(h: &Harness) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT source_character_id FROM core.corp_member_lists WHERE corporation_id = $1",
    )
    .bind(CHRIBBA_CORP)
    .fetch_one(&h.db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn member_requires_the_member_list_scope(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    // A plain login asks EVE for nothing (as in Alliance Auth)...
    let owner = log_in_owner(&h, CHRIBBA).await;
    assert!(h.sso.last_requested.lock().unwrap().is_empty());
    assert_eq!(state_of(&h, &owner).await, "Member");
    // ...so a Member is flagged (not demoted) until they register.
    assert_eq!(compliance(&h, &owner).await, (false, false));
    let checklist = page(&h, "/register", &owner).await.body;
    assert!(
        checklist.contains(tether_core::scopes::CORP_MEMBERSHIP),
        "{checklist}"
    );
    // Registering asks for it, and the stored token carries it.
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    let asked = h.sso.last_requested.lock().unwrap().clone();
    assert!(
        asked.contains(&tether_core::scopes::CORP_MEMBERSHIP.to_owned()),
        "{asked:?}"
    );
    let scopes: Vec<String> =
        sqlx::query_scalar("SELECT scopes FROM core.character_tokens WHERE character_id = $1")
            .bind(CHRIBBA_ID)
            .fetch_one(&h.db)
            .await
            .unwrap();
    assert!(
        scopes.contains(&tether_core::scopes::CORP_MEMBERSHIP.to_owned()),
        "{scopes:?}"
    );
    assert_eq!(compliance(&h, &owner).await, (true, true));
    // Member only: Blue and Guest require nothing of their own.
    let guest = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(state_of(&h, &guest).await, "Guest");
    assert_eq!(compliance(&h, &guest).await, (true, false));
    let states = page(&h, "/admin/states", &owner).await.body;
    assert!(states.contains("needed by Corporation Stats"), "{states}");
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn corp_stats_lists_members_who_never_registered(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    Mock::given(method("GET"))
        .and(path(format!("/corporations/{CHRIBBA_CORP}/members")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([CHRIBBA_ID, 90000011, 90000012])),
        )
        .mount(&h.esi_server)
        .await;

    // Chribba's corporation has pilots here but nobody registered yet, so
    // there's nothing to read it with.
    assert_eq!(
        tether_web::compliance::corp_stats(&h.db, &h.esi, &h.vault)
            .await
            .unwrap(),
        0
    );
    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(officers.contains("No member list yet"), "{officers}");
    // Nothing to offer or approve any more.
    let profile = page(&h, "/dashboard", &owner).await.body;
    assert!(!profile.contains("Share a member list"), "{profile}");
    assert!(!officers.contains("Member list sources"), "{officers}");

    // Registering is enough: no offer, no approval. The corporation's list
    // is read at once (and daily after that).
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    wait_for_member_list_job(&h, CHRIBBA_CORP).await;
    run_jobs(&h).await;
    assert_eq!(member_list_source(&h).await, Some(CHRIBBA_ID));

    let officers = page(&h, "/compliance", &owner).await.body;
    assert!(
        officers.contains(&format!("/corpstats/{CHRIBBA_CORP}")),
        "{officers}"
    );
    assert!(!officers.contains("No member list yet"), "{officers}");
    // Corporation Stats: AA's tabs.
    let list = page(&h, "/corpstats", &owner).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let corp = page(&h, &format!("/corpstats/{CHRIBBA_CORP}"), &owner)
        .await
        .body;
    assert!(corp.contains("Updated "), "{corp}");
    let unregistered = page(
        &h,
        &format!("/corpstats/{CHRIBBA_CORP}?tab=unregistered"),
        &owner,
    )
    .await
    .body;
    assert!(unregistered.contains("Unknown character"), "{unregistered}");
    // Chribba registered: not in the table (the sidebar names the viewer).
    assert!(
        !unregistered.contains(r#"<span class="font-medium">Chribba</span>"#),
        "{unregistered}"
    );
    let mains = page(&h, &format!("/corpstats/{CHRIBBA_CORP}?tab=mains"), &owner)
        .await
        .body;
    assert!(mains.contains("Chribba"), "{mains}");
    let found = page(&h, "/corpstats?q=chrib", &owner).await.body;
    assert!(
        found.contains("Search results") && found.contains("Chribba"),
        "{found}"
    );
    // Update Now queues one refresh of that corporation, at most every 15
    // minutes (the read at registration was an hour ago, say).
    sqlx::query("UPDATE core.jobs SET finished_at = now() - interval '1 hour'")
        .execute(&h.db)
        .await
        .unwrap();
    let res = send(
        &h.app,
        form(&format!("/corpstats/{CHRIBBA_CORP}/update"), "", &owner),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let again = send(
        &h.app,
        form(&format!("/corpstats/{CHRIBBA_CORP}/update"), "", &owner),
    )
    .await;
    assert!(again.body.contains("already waiting"), "{}", again.body);
    // Without a Corporation Stats permission: forbidden; with only
    // view_corp for another corporation: not listed.
    let pilot = log_in_as(&h, "1887431749:gigX", None).await;
    assert_eq!(
        page(&h, "/corpstats", &pilot).await.status,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO core.permission_grants (permission, state_id) VALUES ('corpstats.view_corp_corpstats', $1)")
        .bind(GUEST_STATE)
        .execute(&h.db)
        .await
        .unwrap();
    let theirs = page(&h, "/corpstats", &pilot).await;
    assert_eq!(theirs.status, StatusCode::OK, "{}", theirs.body);
    assert!(
        !theirs.body.contains(&format!("/corpstats/{CHRIBBA_CORP}")),
        "{}",
        theirs.body
    );
    assert_eq!(
        page(&h, &format!("/corpstats/{CHRIBBA_CORP}"), &pilot)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &h.app,
            form(&format!("/corpstats/{CHRIBBA_CORP}/update"), "", &pilot)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Seeing a corporation isn't enough to refresh it (AA: officers or the
    // owner of the token that read it).
    sqlx::query("INSERT INTO core.permission_grants (permission, state_id) VALUES ('corpstats.view_state_corpstats', $1)")
        .bind(GUEST_STATE)
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO core.state_entities (state_id, entity_id, entity_kind, name) VALUES ($1, $2, 'corporation', 'x')")
        .bind(GUEST_STATE)
        .bind(CHRIBBA_CORP)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(
        page(&h, &format!("/corpstats/{CHRIBBA_CORP}"), &pilot)
            .await
            .status,
        StatusCode::OK
    );
    let refused = send(
        &h.app,
        form(&format!("/corpstats/{CHRIBBA_CORP}/update"), "", &pilot),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let lists: i64 = sqlx::query_scalar("SELECT members::bigint FROM core.corp_member_lists")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(lists, 3);

    // Once no state covers the corporation, its list goes at the next run.
    sqlx::query("DELETE FROM core.state_entities WHERE state_id = $1 AND entity_id = 159826257")
        .bind(MEMBER_STATE)
        .execute(&h.db)
        .await
        .unwrap();
    sqlx::query("UPDATE core.accounts SET state_id = $1")
        .bind(GUEST_STATE)
        .execute(&h.db)
        .await
        .unwrap();
    tether_web::compliance::corp_stats(&h.db, &h.esi, &h.vault)
        .await
        .unwrap();
    let lists: i64 = sqlx::query_scalar("SELECT count(*) FROM core.corp_member_lists")
        .fetch_one(&h.db)
        .await
        .unwrap();
    assert_eq!(lists, 0);
}

async fn list_size(h: &Harness) -> i64 {
    sqlx::query_scalar("SELECT members::bigint FROM core.corp_member_lists")
        .fetch_one(&h.db)
        .await
        .unwrap()
}

async fn set_list_source(h: &Harness, character: i64) {
    sqlx::query("UPDATE core.corp_member_lists SET source_character_id = $1")
        .bind(character)
        .execute(&h.db)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn corp_stats_tries_the_last_reader_first_and_skips_one_that_fails(db: PgPool) {
    const MITTANI_ID: i64 = 443630591;
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    // The Mittani, an alt, registers too, and joins Chribba's corporation.
    let owner = log_in_as(&h, MITTANI, Some(&owner)).await;
    assert_eq!(compliance(&h, &owner).await, (true, true));
    sqlx::query("UPDATE core.characters SET corporation_id = $1 WHERE id = $2")
        .bind(CHRIBBA_CORP)
        .bind(MITTANI_ID)
        .execute(&h.db)
        .await
        .unwrap();
    // Each token reads a list (of a different size, to tell them apart).
    let members = format!("/corporations/{CHRIBBA_CORP}/members");
    let bearer = |id: i64| format!("Bearer access-{id}-login");
    Mock::given(method("GET"))
        .and(path(members.clone()))
        .and(wiremock::matchers::header(
            "authorization",
            bearer(CHRIBBA_ID).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([CHRIBBA_ID])))
        .mount(&h.esi_server)
        .await;
    Mock::given(method("GET"))
        .and(path(members.clone()))
        .and(wiremock::matchers::header(
            "authorization",
            bearer(MITTANI_ID).as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([CHRIBBA_ID, MITTANI_ID])),
        )
        .mount(&h.esi_server)
        .await;
    let run = || tether_web::compliance::corp_stats(&h.db, &h.esi, &h.vault);

    // The first read uses one of them, and records which.
    assert_eq!(run().await.unwrap(), 1);
    assert_eq!(member_list_source(&h).await, Some(CHRIBBA_ID));
    assert_eq!(list_size(&h).await, 1);
    // Whichever read it last is tried first.
    set_list_source(&h, MITTANI_ID).await;
    assert_eq!(run().await.unwrap(), 1);
    assert_eq!(member_list_source(&h).await, Some(MITTANI_ID));
    assert_eq!(list_size(&h).await, 2);

    // Chribba's stops working (ESI refuses it): skipped for The Mittani's,
    // and nobody is told (nobody volunteered it).
    Mock::given(method("GET"))
        .and(path(members.clone()))
        .and(wiremock::matchers::header(
            "authorization",
            bearer(CHRIBBA_ID).as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(serde_json::json!({"error": "forbidden"})),
        )
        .with_priority(1)
        .mount(&h.esi_server)
        .await;
    set_list_source(&h, CHRIBBA_ID).await;
    assert_eq!(run().await.unwrap(), 1);
    assert_eq!(member_list_source(&h).await, Some(MITTANI_ID));
    assert_eq!(list_size(&h).await, 2);
    let told: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM core.notifications WHERE title LIKE 'Corporation Stats%'",
    )
    .fetch_one(&h.db)
    .await
    .unwrap();
    assert_eq!(told, 0);

    // ESI down isn't a member's fault: the run stops (and is retried)
    // rather than trying every member's token in turn.
    let outage = Mock::given(method("GET"))
        .and(path(members))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .expect(1)
        .mount_as_scoped(&h.esi_server)
        .await;
    set_list_source(&h, MITTANI_ID).await;
    assert!(run().await.is_err());
    drop(outage);

    // A token without the scope isn't used at all.
    sqlx::query("UPDATE core.character_tokens SET scopes = '{}' WHERE character_id = $1")
        .bind(MITTANI_ID)
        .execute(&h.db)
        .await
        .unwrap();
    assert_eq!(run().await.unwrap(), 0);
    assert_eq!(member_list_source(&h).await, Some(MITTANI_ID));
}

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn tether_manages_the_compliant_group(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    let group: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE compliance")
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
        tether_core::groups::Flags {
            internal: true,
            hidden: true,
            ..Default::default()
        },
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
    let compliant: i64 = sqlx::query_scalar("SELECT id FROM core.groups WHERE compliance")
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

#[sqlx::test(migrator = "tether_db::MIGRATOR")]
async fn admins_designate_compliance_groups_per_state(db: PgPool) {
    cover(&db, Builtin::Member, EntityKind::Alliance, 159826257).await;
    cover(&db, Builtin::Blue, EntityKind::Corporation, 98133756).await;
    let h = harness(db, true).await;
    let owner = log_in_owner(&h, CHRIBBA).await;
    let owner = round_trip(&h, &owner, "/register/start", CHRIBBA).await;
    let blue = log_in_as(&h, "1887431749:gigX", None).await;
    let res = send(
        &h.app,
        post_json("/api/admin/groups", &owner, r#"{"name":"Blue Compliant"}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.body);
    let id: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    let id = id["id"].as_i64().unwrap();

    let body = format!(
        r#"{{"internal":true,"hidden":true,"open":false,"public":false,"restricted":false,"compliance":true,"states":[{BLUE_STATE}]}}"#
    );
    let res = send(
        &h.app,
        Request::put(format!("/api/admin/groups/{id}"))
            .header(header::ORIGIN, SITE)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, format!("{SESSION}={owner}"))
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.body);
    run_jobs(&h).await;

    let groups = |token: String| {
        let h = &h;
        async move { me(h, &token).await["groups"].clone() }
    };
    // The Blue pilot is in it (and in Compliant, which takes every state
    // but Guest); the Member owner only in Compliant.
    assert_eq!(
        groups(blue.clone()).await,
        serde_json::json!(["Blue Compliant", "Compliant"])
    );
    assert_eq!(
        groups(owner.clone()).await,
        serde_json::json!(["Compliant"])
    );
}
